//! `BlockioSensor` — handgeschriebene `Sensor`-Implementierung für
//! `<block-wurzel>/<gerät>/stat` (AW2-10-Arbeitsauftrag).
//!
//! Siehe die Crate-Moduldoku (`lib.rs`) für die Quellwahl (sysfs statt
//! `/proc/diskstats`), das Zählerformat, die zwei bewussten Entscheidungen
//! (kumulative Zähler statt Rate; Geräteauswahl) und die Begründung, warum
//! dieser Sensor `Sensor` von Hand statt über
//! `#[derive(harw_macros::SensorSource)]` implementiert. Dieses Modul
//! besitzt ausschließlich [`BlockioSensor`] und die private Lese-,
//! Filter- und Zuordnungslogik dahinter; kein `std::fs`-Aufruf steht in
//! dieser Datei — jeder Zugriff läuft über `harw_dod_readfs`
//! (`read_line_fields`, `glob::glob`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Suffix-Glob-Muster für die Geräteverzeichnisse, relativ zur ersten
/// Bereichswurzel (siehe `relative_pattern` für die Begründung, warum das
/// volle Muster erst zur Laufzeit zusammengesetzt wird, statt fest
/// verdrahtet zu sein — Crate-Moduldoku, Abschnitt „Warum kein
/// `#[derive(SensorSource)]`").
///
/// Ein einzelnes `*` adressiert genau eine Verzeichnisebene: jedes direkte
/// Unterverzeichnis der Bereichswurzel gilt als ein Gerät, dessen `stat`-
/// Datei gelesen wird. Passt sowohl auf die Produktionsform
/// (`/sys/block/sda/stat`) als auch auf die Fixture-Form
/// (`fixtures/<fall>/tree/sda/stat`).
const DEVICE_STAT_GLOB_SUFFIX: &str = "*/stat";

/// Mindestzahl numerischer Felder einer `stat`-Zeile, unterhalb derer die
/// Zeile als fehlerhaft gilt.
///
/// # Description
/// Elf Felder (`read_ios` … `time_in_queue_ms`) trägt die Statistik bereits
/// in ihrer ursprünglichen Form; jede gültige `stat`-Datei hat mindestens
/// diese elf (siehe `lib.rs`-Moduldoku, Abschnitt „Das Zählerformat"). Eine
/// Zeile mit weniger Feldern ist keine gültige Ausgabe irgendeiner bekannten
/// Kernel-Version.
pub const MIN_STAT_FIELDS: usize = 11;

/// Die bis zu siebzehn bekannten Spaltennamen einer `stat`-Zeile, in exakter
/// Reihenfolge, als Präfix für jeden `HostSample::metric`-Wert (siehe
/// [`sanitize_device_label`] für das angehängte Gerätesuffix).
///
/// # Description
/// Enthält eine Zeile mehr als siebzehn numerische Felder (ein künftiger
/// Kernel mit weiteren Spalten), werden die überzähligen Felder
/// stillschweigend ignoriert — dieser Sensor erfindet für sie keinen
/// Metriknamen, den er nicht kennt (siehe `lib.rs`-Moduldoku, Abschnitt
/// „Das Zählerformat").
pub const FIELD_METRICS: [&str; 17] = [
    "read_ios",
    "read_merges",
    "read_sectors",
    "read_ticks_ms",
    "write_ios",
    "write_merges",
    "write_sectors",
    "write_ticks_ms",
    "io_in_flight",
    "io_ticks_ms",
    "time_in_queue_ms",
    "discard_ios",
    "discard_merges",
    "discard_sectors",
    "discard_ticks_ms",
    "flush_ios",
    "flush_ticks_ms",
];

/// Namenspräfixe virtueller Blockgeräte, die dieser Sensor nie meldet (siehe
/// `lib.rs`-Moduldoku, Entscheidung 2, Teilentscheidung 1).
///
/// # Description
/// `loop*`, `ram*`, `zram*`, `dm-*` und `sr*` können auf einem Host in einer
/// Zahl existieren, die nichts mit dessen physischer Datenträgerausstattung
/// zu tun hat — genau die Kardinalitätsfalle, gegen die diese Ausschlussliste
/// bewusst gebaut ist.
const NOISE_PREFIXES: [&str; 5] = ["loop", "ram", "zram", "dm-", "sr"];

/// Obergrenze der gemeldeten Geräte je Poll (siehe `lib.rs`-Moduldoku,
/// Entscheidung 2, Teilentscheidung 3).
///
/// # Description
/// Nach Rauschen- und Partitionsfilterung verbleibende Gerätenamen werden
/// alphabetisch sortiert (Ergebnis von [`harw_dod_readfs::glob::glob`], das
/// seine Treffer sortiert liefert) und auf die ersten `MAX_DEVICES`
/// abgeschnitten — deterministisch, aber mit der dokumentierten
/// Einschränkung, dass ein Host mit mehr als `MAX_DEVICES` physischen
/// Datenträgern die alphabetisch spätesten davon nicht gemeldet bekommt.
pub const MAX_DEVICES: usize = 32;

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll.
///
/// # Description
/// `MAX_DEVICES` Geräte mal [`FIELD_METRICS`]`.len()` Felder je Gerät — der
/// Fixture-seitige Spiegel, den [`harw_dod_fixtures::sensor_suite!`] als
/// `max_cardinality` erwartet.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_blockio::sensor::MAX_CARDINALITY, 32 * 17);
/// ```
pub const MAX_CARDINALITY: usize = MAX_DEVICES * FIELD_METRICS.len();

/// Obergrenze der Zeichen, die aus einem Geräte-Verzeichnisnamen in den
/// Metriknamen übernommen werden — Schutz gegen unbegrenztes Wachstum durch
/// einen ungewöhnlichen Namen (Adversarial-Fall, analog zu
/// `harw-dod-thermal`s Zonenlabel).
const MAX_LABEL_LEN: usize = 32;

/// Ersatzlabel, falls ein Geräte-Verzeichnisname nach der Sanitisierung leer
/// bliebe (praktisch unerreichbar bei sysfs-/Fixture-Gerätenamen, aber total
/// statt `unwrap`).
const FALLBACK_DEVICE_LABEL: &str = "device";

/// Blockgeräte-Sensor: liest `stat` jedes sichtbaren, nicht ausgeschlossenen
/// Geräts unterhalb der Bereichswurzel.
///
/// # Description
/// Hält ausschließlich den gebundenen Griff; die gesamte Lese-, Filter- und
/// Zuordnungslogik steht in [`Sensor::poll`]. Implementiert `Sensor` von
/// Hand statt über `#[derive(harw_macros::SensorSource)]` — siehe
/// `lib.rs`-Moduldoku, Abschnitt „Warum kein `#[derive(SensorSource)]`",
/// für die vollständige Begründung.
#[derive(Debug)]
pub struct BlockioSensor {
    handle: SensorHandle<Bound>,
}

impl BlockioSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration für das CI-Privilegienbudget-Gate,
    /// nachgebildet aus dem, was `#[derive(harw_macros::SensorSource)]` an
    /// derselben Stelle erzeugen würde (siehe `lib.rs`-Moduldoku).
    ///
    /// # Returns
    /// [`Capability::ReadSysfsBlock`], die einzige Fähigkeit dieses Sensors.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_blockio::BlockioSensor;
    ///
    /// assert_eq!(BlockioSensor::CAPABILITY, Capability::ReadSysfsBlock);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadSysfsBlock;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Rein informativ, analog zu dem, was
    /// `#[derive(harw_macros::SensorSource)]` als `Self::SENSOR_ID` erzeugen
    /// würde. Nicht zu verwechseln mit `harw_dod_cap::SensorHandle::id()`,
    /// der Kennung der konkreten Sensor-*Instanz*.
    ///
    /// # Returns
    /// `"blockio"`.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_blockio::BlockioSensor;
    ///
    /// assert_eq!(BlockioSensor::SENSOR_ID, "blockio");
    /// ```
    pub const SENSOR_ID: &'static str = "blockio";
}

impl From<SensorHandle<Bound>> for BlockioSensor {
    /// Baut einen `BlockioSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige von `harw_dod_fixtures::sensor_suite!` verlangte
    /// Konstruktionskonvention (siehe dessen Moduldoku): ein gebundener
    /// Griff hinein, eine Sensor-Instanz heraus.
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, mit
    ///   [`Capability::ReadSysfsBlock`] und einem `ReadScope` auf die
    ///   Blockgeräte-Wurzel (real: `/sys/block`; in Tests: ein
    ///   Fixture-`tree/`-Verzeichnis).
    ///
    /// # Returns
    /// Einen einsatzbereiten `BlockioSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::AliasRoot;
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_blockio::BlockioSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// // `/sys/block/*`-Einträge sind Symlinks nach `/sys/devices/...`
    /// // (F-005) — `AliasRoot::sysfs_class` baut den Bereich, der solche
    /// // Ziele zulässt, statt der für sysfs-Klassenwurzeln unsicheren
    /// // `ReadScope::from_roots`.
    /// let alias =
    ///     AliasRoot::sysfs_class(PathBuf::from("/sys/block")).expect("gültige sysfs-Klassenwurzel");
    /// let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
    /// let handle = SensorHandle::new(SensorId::from_str("blockio-0"), Capability::ReadSysfsBlock)
    ///     .bind(scope);
    /// let _sensor = BlockioSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for BlockioSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei [`BlockioSensor::from`] übergebenen
    /// `SensorHandle`.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest die `stat`-Datei jedes sichtbaren, nicht ausgeschlossenen
    /// Geräts einmal aus.
    ///
    /// # Description
    /// Ermittelt zuerst alle `<gerät>/stat`-Pfade unterhalb der ersten
    /// Bereichswurzel ([`DEVICE_STAT_GLOB_SUFFIX`], siehe `lib.rs`-Moduldoku
    /// für die Begründung, warum das Muster zur Laufzeit gebaut wird).
    /// Schließt virtuelle Rauschgeräte aus ([`is_noise_device`]; **keine**
    /// gesonderte Partitionsfilterung mehr — F-204, siehe `lib.rs`-Moduldoku,
    /// Entscheidung 2, für die Begründung, warum `DEVICE_STAT_GLOB_SUFFIX`
    /// bereits strukturell keine Partitionen trifft), begrenzt die
    /// verbleibenden Geräte auf [`MAX_DEVICES`]. Für jedes
    /// verbleibende Gerät: liest dessen `stat`-Zeile und ordnet ihre Felder
    /// [`FIELD_METRICS`] zu ([`map_stat_fields`]), angehängt an das
    /// sanitisierte Gerätelabel ([`sanitize_device_label`]).
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert in jedes
    ///   `HostSample::observed_at` übernommen. Diese Funktion liest nie die
    ///   Systemuhr.
    ///
    /// # Returns
    /// Ein `SensorReading` mit bis zu `MAX_DEVICES × 17` Samples und keinen
    /// Events (Blockgeräte-Zähler erzeugen keine Sicherheitsereignisse).
    /// Nie leer bei `Ok` — eine leere Gerätemenge nach Filterung ist
    /// [`SensorError::SourceUnavailable`], kein leeres `Ok` (analog zu
    /// `harw-dod-thermal`s Zonenmenge).
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: der Lesebereich hat keine
    ///   Wurzel, keine `<gerät>/stat`-Datei ist sichtbar, oder nach
    ///   Rauschen-/Partitionsfilterung bleibt kein Gerät übrig.
    /// - [`SensorError::MalformedSource`]: die `stat`-Zeile eines
    ///   verbleibenden Geräts hat weniger als [`MIN_STAT_FIELDS`] Felder,
    ///   eines davon ist keine gültige `u64`-Zahl, ist leer, oder
    ///   überschreitet `harw_dod_readfs::MAX_READ_BYTES`. **Der nicht
    ///   passende Zeileninhalt erscheint in keinem dieser Fälle in der
    ///   Fehlermeldung.**
    /// - [`SensorError::OutsideScope`] / [`SensorError::Io`]: unverändert
    ///   durchgereicht, falls Glob oder Lesevorgang einen anderen
    ///   Bereichsfehler als „nicht gefunden" melden.
    ///
    /// # Examples
    /// Siehe die Crate-Moduldoku für ein vollständiges Beispiel.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let Some(root) = scope.roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let Some(pattern) = relative_pattern(root, DEVICE_STAT_GLOB_SUFFIX) else {
            return Err(SensorError::SourceUnavailable);
        };

        let stat_paths = harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_readfs_err)?;
        if stat_paths.is_empty() {
            return Err(SensorError::SourceUnavailable);
        }

        // Gerätename (Verzeichnisname der `stat`-Datei) neben ihrem Pfad
        // behalten; ein Treffer ohne auswertbaren Verzeichnisnamen wird
        // stillschweigend übersprungen statt die gesamte Suche scheitern zu
        // lassen (praktisch unerreichbar bei sysfs-/Fixture-Pfaden).
        let mut named: Vec<(String, PathBuf)> = stat_paths
            .into_iter()
            .filter_map(|path| {
                let name = path.parent()?.file_name()?.to_str()?.to_owned();
                Some((name, path))
            })
            .collect();

        named.retain(|(name, _)| !is_noise_device(name));

        // Keine Partitionsfilterung mehr (F-204): `DEVICE_STAT_GLOB_SUFFIX`
        // (`*/stat`) passt nur eine einzige Verzeichnisebene unterhalb der
        // Bereichswurzel — echte Partitionen liegen unter `/sys/block` aber
        // stets eine Ebene *tiefer*, im Verzeichnis ihres Ganzgeräts
        // (`/sys/block/sda/sda1/stat`, nicht `/sys/block/sda1/stat`). Das
        // frühere `is_partition_of_any` beruhte auf der falschen Annahme,
        // `/sys/block` liste Partitionen als Geschwister ihres Ganzgeräts,
        // und erzeugte deshalb nur Fehlklassifikationen ohne echten Nutzen
        // (z. B. `nvme0n10` fälschlich als „Partition 0" von `nvme0n1`
        // ausgeschlossen) — siehe `lib.rs`-Moduldoku, Entscheidung 2, für die
        // vollständige Herleitung.
        named.truncate(MAX_DEVICES);

        if named.is_empty() {
            return Err(SensorError::SourceUnavailable);
        }

        let mut samples = Vec::with_capacity(named.len() * FIELD_METRICS.len());
        for (device, stat_path) in &named {
            let rows =
                harw_dod_readfs::read_line_fields(scope, stat_path).map_err(map_readfs_err)?;
            let fields = map_stat_fields(&rows)?;
            let label = sanitize_device_label(device);

            for (metric, value) in fields {
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{metric}_{label}")),
                    value: value as f64,
                });
            }
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
// vollen Pfad fest zu verdrahten (siehe `lib.rs`-Moduldoku, Abschnitt „Warum
// kein `#[derive(SensorSource)]`" für die Begründung). Unabhängige Kopie der
// gleichnamigen Funktion in `harw-dod-thermal` — C7 verbietet eine
// Abhängigkeit zwischen Sensor-Crates, auch für zehn identische Zeilen.
fn relative_pattern(root: &Path, suffix: &str) -> Option<String> {
    let relative = root.strip_prefix("/").ok()?;
    let relative = relative.to_string_lossy();
    if relative.is_empty() {
        Some(suffix.to_owned())
    } else {
        Some(format!("{relative}/{suffix}"))
    }
}

/// Ist `name` ein virtuelles Rauschgerät, das dieser Sensor nie meldet?
///
/// # Description
/// Prüft, ob `name` mit einem der [`NOISE_PREFIXES`] beginnt (siehe
/// `lib.rs`-Moduldoku, Entscheidung 2, Teilentscheidung 1, für die
/// Begründung je Präfix).
///
/// # Arguments
/// - `name` (`&str`): der zu prüfende Gerätename (Verzeichnisname).
///
/// # Returns
/// `true`, wenn `name` mit einem bekannten Rauschgeräte-Präfix beginnt.
///
/// # Examples
/// ```rust
/// use harw_dod_blockio::sensor::is_noise_device;
///
/// assert!(is_noise_device("loop0"));
/// assert!(is_noise_device("dm-0"));
/// assert!(!is_noise_device("sda"));
/// assert!(!is_noise_device("nvme0n1"));
/// ```
#[must_use]
pub fn is_noise_device(name: &str) -> bool {
    NOISE_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

// F-204: `is_partition_suffix`/`is_partition_of_any` (Koexistenz-Heuristik
// über sichtbare Gerätenamen) wurden entfernt. Sie beruhten auf der falschen
// Annahme, `/sys/block` liste Partitionen als Geschwister ihres Ganzgeräts
// (`sda`, `sda1` beide direkt unter der Bereichswurzel) — tatsächlich listet
// `/sys/block` ausschließlich Ganzgeräte; echte Partitionen liegen genau eine
// Ebene tiefer, im Verzeichnis ihres Ganzgeräts (`sda/sda1`), und werden von
// [`DEVICE_STAT_GLOB_SUFFIX`] (`*/stat`, eine einzige Ebene) strukturell nie
// getroffen. Die Heuristik erzeugte deshalb nur Fehlklassifikationen ohne
// echten Nutzen, zum Beispiel `nvme0n10` (ein eigenständiges, physisches
// Gerät) fälschlich als „Partition 0" von `nvme0n1` ausgeschlossen. Siehe
// `lib.rs`-Moduldoku, Entscheidung 2, für die vollständige Herleitung.

/// Reduziert einen Geräte-Verzeichnisnamen auf sichere Metriknamen-Zeichen
/// (`[a-z0-9_]`), gekürzt auf [`MAX_LABEL_LEN`] Zeichen.
///
/// # Description
/// Adversarial-Schutz, analog zu `harw-dod-thermal`s Zonenlabel-
/// Sanitisierung: ein ungewöhnlicher Verzeichnisname darf den Metriknamen
/// weder unbegrenzt wachsen lassen noch Zeichen einschleusen, die als
/// Metrikname untypisch wären. Sysfs-Gerätenamen bestehen in der Praxis
/// bereits ausschließlich aus `[a-z0-9-]` (siehe `NOISE_PREFIXES`s eigene
/// Beispiele) — diese Funktion ist dennoch total, nicht nur für den
/// erwarteten Fall.
///
/// # Arguments
/// - `raw` (`&str`): der ungeprüfte Geräte-Verzeichnisname.
///
/// # Returns
/// Das sanitisierte Label, nie leer (siehe [`FALLBACK_DEVICE_LABEL`]).
///
/// # Examples
/// ```rust
/// use harw_dod_blockio::sensor::sanitize_device_label;
///
/// assert_eq!(sanitize_device_label("sda"), "sda");
/// assert_eq!(sanitize_device_label("nvme0n1"), "nvme0n1");
/// ```
#[must_use]
pub fn sanitize_device_label(raw: &str) -> String {
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
        FALLBACK_DEVICE_LABEL.to_owned()
    } else {
        sanitized
    }
}

/// Ordnet die Felder einer `stat`-Zeile [`FIELD_METRICS`] zu.
///
/// # Description
/// `rows` ist das Ergebnis von [`harw_dod_readfs::read_line_fields`]: je
/// Zeile ein Vektor ihrer leerraumgetrennten Felder, ohne Prüfung der
/// Feldzahl (siehe dessen Moduldoku). Eine `stat`-Datei trägt genau eine
/// nicht-leere Zeile; diese Funktion nimmt deren erste Zeile und ordnet ihre
/// Felder positionsweise [`FIELD_METRICS`] zu. Überzählige Felder über
/// [`FIELD_METRICS`]`.len()` hinaus werden stillschweigend ignoriert — der
/// Robustheitsfall, den ein Kernel mit weiteren Spalten auslöst (siehe
/// `lib.rs`-Moduldoku, Abschnitt „Das Zählerformat").
///
/// # Arguments
/// - `rows` (`&[Vec<String>]`): die Zeilen einer `stat`-Datei, wie von
///   [`harw_dod_readfs::read_line_fields`] geliefert.
///
/// # Returns
/// Die zugeordneten `(Metrikname, Wert)`-Paare, in Spaltenreihenfolge —
/// mindestens [`MIN_STAT_FIELDS`], höchstens [`FIELD_METRICS`]`.len()`
/// Einträge.
///
/// # Errors
/// [`SensorError::MalformedSource`], wenn `rows` leer ist (leere Datei),
/// die erste Zeile weniger als [`MIN_STAT_FIELDS`] Felder hat, oder eines
/// dieser Felder keine gültige `u64`-Zahl ist. Der nicht passende
/// Zeileninhalt erscheint nie in der Fehlermeldung —
/// [`SensorError::MalformedSource`] ist inhaltsfrei.
///
/// # Examples
/// ```rust
/// use harw_dod_blockio::sensor::map_stat_fields;
///
/// let rows = vec![vec![
///     "1".to_owned(), "2".to_owned(), "3".to_owned(), "4".to_owned(),
///     "5".to_owned(), "6".to_owned(), "7".to_owned(), "8".to_owned(),
///     "9".to_owned(), "10".to_owned(), "11".to_owned(),
/// ]];
/// let fields = map_stat_fields(&rows).expect("elf Felder müssen gelingen");
/// assert_eq!(fields.len(), 11);
/// assert_eq!(fields[0], ("read_ios", 1));
/// ```
pub fn map_stat_fields(rows: &[Vec<String>]) -> Result<Vec<(&'static str, u64)>, SensorError> {
    let fields = rows.first().ok_or(SensorError::MalformedSource)?;
    if fields.len() < MIN_STAT_FIELDS {
        return Err(SensorError::MalformedSource);
    }

    FIELD_METRICS
        .iter()
        .zip(fields.iter())
        .map(|(metric, raw)| {
            raw.trim()
                .parse::<u64>()
                .map(|value| (*metric, value))
                .map_err(|_| SensorError::MalformedSource)
        })
        .collect()
}

// Bildet einen `harw_dod_readfs::ReadFsError` auf `SensorError` ab.
// Unabhängige Kopie der gleichnamigen Funktion in `harw-dod-thermal` und
// `harw-dod-cpu` (C7 verbietet eine Abhängigkeit zwischen Sensor-Crates).
fn map_readfs_err(err: ReadFsError) -> SensorError {
    match err {
        ReadFsError::Scope(inner) => inner,
        ReadFsError::TooLarge { .. }
        | ReadFsError::GlobPatternAbsolute { .. }
        | ReadFsError::GlobPatternTraversal { .. }
        // Eine überschrittene Glob-Grenze beschreibt, wie `TooLarge`, eine
        // Quelle mit unerwarteter Form (ungewöhnlich viele/tiefe
        // Geräteverzeichnisse), nicht einen Fehler dieses Werkzeugs
        // (C-SCOPE-Nachfolge, F-005-Register).
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

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Das `fixtures/`-Wurzelverzeichnis dieser Crate.
    fn fixtures_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    /// Baut einen `BlockioSensor`, dessen `ReadScope` genau `root` umfasst.
    fn build_sensor(root: PathBuf) -> BlockioSensor {
        let scope = ReadScope::from_roots([root]);
        let handle = SensorHandle::new(
            SensorId::from_str("blockio-test"),
            Capability::ReadSysfsBlock,
        )
        .bind(scope);
        BlockioSensor::from(handle)
    }

    // ---- Feldzuordnung (`map_stat_fields`) ----

    fn rows_from(fields: &[&str]) -> Vec<Vec<String>> {
        vec![fields.iter().map(|s| (*s).to_owned()).collect()]
    }

    #[test]
    fn test_map_stat_fields_maps_middle_column_not_just_the_first() -> TestResult {
        let rows = rows_from(&[
            "1234", "56", "78901", "2345", "6789", "12", "34567", "890", "0", "1111", "2222",
        ]);
        let fields =
            map_stat_fields(&rows).map_err(ctx("elf wohlgeformte Felder müssen gelingen"))?;

        // "write_ios" ist das fünfte gemeldete Feld (Index 4) — der Wert aus
        // der Mitte der Zeile, nicht der erste oder letzte.
        assert_eq!(fields[4], ("write_ios", 6789));
        assert_eq!(fields[0], ("read_ios", 1234));
        assert_eq!(fields[10], ("time_in_queue_ms", 2222));
        Ok(())
    }

    #[test]
    fn test_map_stat_fields_accepts_seventeen_fields_not_just_eleven() -> TestResult {
        // Der wichtigste Robustheitsfall: ein Kernel ab 4.18/5.5 hängt sechs
        // weitere Spalten (Discard, Flush) an. Der Parser darf daran nicht
        // scheitern.
        let rows = rows_from(&[
            "1000", "10", "20000", "3000", "4000", "40", "50000", "6000", "2", "7000", "8000",
            "100", "5", "600", "70", "9", "800",
        ]);
        let fields =
            map_stat_fields(&rows).map_err(ctx("siebzehn Felder dürfen nicht scheitern"))?;
        assert_eq!(fields.len(), FIELD_METRICS.len());
        assert_eq!(fields[16], ("flush_ticks_ms", 800));
        Ok(())
    }

    #[test]
    fn test_map_stat_fields_accepts_minimum_eleven_fields() -> TestResult {
        let rows = rows_from(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"]);
        let fields =
            map_stat_fields(&rows).map_err(ctx("elf Felder sind die historische Untergrenze"))?;
        assert_eq!(fields.len(), 11);
        Ok(())
    }

    #[test]
    fn test_map_stat_fields_rejects_too_few_fields_without_leaking_content() -> TestResult {
        let rows = rows_from(&["1", "2", "3"]);
        let Err(err) = map_stat_fields(&rows) else {
            return Err(TestError::Unexpected(
                "drei Felder unterschreiten die Untergrenze".to_owned(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        assert!(!err.to_string().contains("1 2 3"));
        Ok(())
    }

    #[test]
    fn test_map_stat_fields_rejects_non_numeric_field_without_leaking_content() -> TestResult {
        let rows = rows_from(&[
            "1",
            "2",
            "not-a-number",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "10",
            "11",
        ]);
        let Err(err) = map_stat_fields(&rows) else {
            return Err(TestError::Unexpected(
                "nicht-numerischer Wert muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        assert!(!err.to_string().contains("not-a-number"));
        Ok(())
    }

    #[test]
    fn test_map_stat_fields_rejects_empty_rows() -> TestResult {
        let rows: Vec<Vec<String>> = Vec::new();
        let Err(err) = map_stat_fields(&rows) else {
            return Err(TestError::Unexpected(
                "leere Zeilenliste muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    // ---- Rauschenfilterung (`is_noise_device`) ----

    #[test]
    fn test_is_noise_device_matches_known_virtual_prefixes() {
        assert!(is_noise_device("loop0"));
        assert!(is_noise_device("ram0"));
        assert!(is_noise_device("zram0"));
        assert!(is_noise_device("dm-0"));
        assert!(is_noise_device("sr0"));
    }

    #[test]
    fn test_is_noise_device_does_not_match_real_disks() {
        assert!(!is_noise_device("sda"));
        assert!(!is_noise_device("nvme0n1"));
        assert!(!is_noise_device("mmcblk0"));
        assert!(!is_noise_device("vda"));
    }

    // F-204: die frühere Partitionserkennung (`is_partition_of_any`) und ihre
    // Tests wurden entfernt — siehe die Begründung über
    // `sanitize_device_label` weiter oben in dieser Datei.
    #[test]
    fn test_device_glob_suffix_matches_exactly_one_level_never_nested_partitions() {
        // Regressionsanker für F-204: das Suffix hat genau einen `*` und
        // keinen weiteren Pfadtrenner — ein zweiter Wildcard-Level (der
        // tatsächliche Ort echter Partitionen, `<gerät>/<gerät>N/stat`) würde
        // hier nie erzeugt werden. Das ist der strukturelle Grund, warum
        // diese Crate keine gesonderte Partitionsfilterung mehr braucht.
        assert_eq!(DEVICE_STAT_GLOB_SUFFIX, "*/stat");
        assert_eq!(DEVICE_STAT_GLOB_SUFFIX.matches('/').count(), 1);
    }

    // ---- Konstanten ----

    #[test]
    fn test_max_cardinality_is_max_devices_times_field_metrics_len() {
        assert_eq!(MAX_CARDINALITY, MAX_DEVICES * FIELD_METRICS.len());
        assert_eq!(MAX_CARDINALITY, 32 * 17);
    }

    #[test]
    fn test_capability_and_sensor_id_constants() {
        assert_eq!(BlockioSensor::CAPABILITY, Capability::ReadSysfsBlock);
        assert_eq!(BlockioSensor::SENSOR_ID, "blockio");
    }

    // ---- Fixture-gestützte Verhaltensprüfungen ----

    #[test]
    fn test_single_device_fixture_reports_eleven_samples() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("single-device/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("single-device Fixture muss erfolgreich pollen"))?;
        assert_eq!(reading.samples.len(), 11);
        assert!(reading.samples.iter().all(|s| s.metric.ends_with("_sda")));
        Ok(())
    }

    #[test]
    fn test_seventeen_fields_fixture_is_read_not_rejected() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("seventeen-fields/tree"));
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "eine Zeile mit siebzehn Feldern darf nicht abgewiesen werden",
        ))?;
        assert_eq!(reading.samples.len(), 17);
        Ok(())
    }

    #[test]
    fn test_multi_device_fixture_excludes_loop_noise_device() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("multi-device/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("multi-device Fixture muss erfolgreich pollen"))?;

        assert!(
            reading
                .samples
                .iter()
                .all(|s| !s.metric.ends_with("_loop0")),
            "kein Sample darf vom ausgeschlossenen Loop-Gerät stammen: {:?}",
            reading.samples
        );
        Ok(())
    }

    #[test]
    fn test_multi_device_fixture_excludes_partitions_but_keeps_whole_disks() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("multi-device/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("multi-device Fixture muss erfolgreich pollen"))?;

        assert!(
            reading.samples.iter().all(|s| !s.metric.ends_with("_sda1")),
            "die Partition sda1 darf nicht gemeldet werden: {:?}",
            reading.samples
        );
        assert!(
            reading
                .samples
                .iter()
                .all(|s| !s.metric.ends_with("_nvme0n1p1")),
            "die Partition nvme0n1p1 darf nicht gemeldet werden: {:?}",
            reading.samples
        );
        assert!(
            reading.samples.iter().any(|s| s.metric.ends_with("_sda")),
            "das Ganzgerät sda muss gemeldet werden: {:?}",
            reading.samples
        );
        assert!(
            reading
                .samples
                .iter()
                .any(|s| s.metric.ends_with("_nvme0n1")),
            "das Ganzgerät nvme0n1 muss gemeldet werden: {:?}",
            reading.samples
        );
        Ok(())
    }

    #[test]
    fn test_poll_is_deterministic_for_same_now_on_multi_device_fixture() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("multi-device/tree"));
        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("erster Poll muss gelingen"))?;
        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("zweiter Poll muss gelingen"))?;
        assert_eq!(
            first, second,
            "zwei Polls mit demselben injizierten now müssen identisch sein"
        );
        Ok(())
    }

    #[test]
    fn test_poll_returns_source_unavailable_when_only_noise_devices_present() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir_all(dir.path().join("loop0")).map_err(ctx("loop0 dir"))?;
        std::fs::write(dir.path().join("loop0/stat"), "1 2 3 4 5 6 7 8 9 10 11\n")
            .map_err(ctx("loop0 stat schreiben"))?;

        let sensor = build_sensor(dir.path().to_path_buf());
        let Err(err) = sensor.poll(Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected(
                "ein Baum mit ausschließlich Rauschgeräten darf kein Ok liefern".to_owned(),
            ));
        };
        assert!(matches!(err, SensorError::SourceUnavailable));
        Ok(())
    }

    /// Der Ordner der echten Pi-Captures (`C-FIXT`, `harw-dod-fixtures`),
    /// über den Workspace-Geschwisterpfad erreicht (analog zu `path =
    /// "../harw-dod-cap"` in `Cargo.toml`).
    fn rpi5_captures_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../harw-dod-fixtures/captures/rpi5-6.18"
        ))
    }

    /// Regressionstest für F-005/F-202 mit einer echten, auf diesem
    /// Raspberry Pi 5 erhobenen Capture (`block.json`): `mmcblk0` erscheint
    /// dort als echter Symlink nach `/sys/devices/platform/.../mmcblk0`. Die
    /// Capture wurde unter `/sys/class/block/mmcblk0` erhoben (nicht unter
    /// `/sys/block/mmcblk0`, der Produktionswurzel dieser Crate) — beide
    /// Pfade sind unabhängige, gleichermaßen echte sysfs-Aliaswurzeln auf
    /// denselben Gerätebaum; dieser Test belegt den Resolutionsmechanismus
    /// von [`harw_dod_cap::scope::AliasRoot`] anhand der einzigen
    /// verfügbaren Blockgeräte-Capture, unabhängig vom konkreten
    /// Klassennamen.
    #[test]
    fn test_poll_reads_real_pi_capture_through_alias_scope_regression_f005() -> TestResult {
        let manifest_path = rpi5_captures_dir().join("block.json");
        let manifest = harw_dod_fixtures::capture_manifest::load(&manifest_path)
            .map_err(ctx("captures/rpi5-6.18/block.json muss ladbar sein"))?;

        let tmp = tempfile::tempdir().map_err(ctx("tempdir für die Materialisierung"))?;
        harw_dod_fixtures::capture_manifest::materialize(&manifest, tmp.path())
            .map_err(ctx("materialize muss die echte Symlink-Struktur anlegen"))?;

        let declared = tmp.path().join("sys/class/block");
        let resolved_prefix = tmp.path().join("sys/devices");
        let alias = harw_dod_cap::scope::AliasRoot::new(declared, resolved_prefix)
            .map_err(ctx("AliasRoot::new mit Tempdir-Wurzeln"))?;
        let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
        let handle = SensorHandle::new(
            SensorId::from_str("blockio-alias-capture-test"),
            Capability::ReadSysfsBlock,
        )
        .bind(scope);
        let sensor = BlockioSensor::from(handle);

        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "Alias-Scope muss die reale Pi-Capture über den Symlink lesen",
        ))?;

        assert_eq!(reading.samples.len(), FIELD_METRICS.len());
        assert!(
            reading
                .samples
                .iter()
                .all(|s| s.metric.ends_with("_mmcblk0")),
            "alle Samples müssen das Gerätelabel der echten Capture tragen: {:?}",
            reading.samples
        );
        Ok(())
    }
}
