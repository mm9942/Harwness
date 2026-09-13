//! `NetCountersSensor` — handgeschriebene `Sensor`-Implementierung für
//! `/proc/net/dev`.
//!
//! # Verantwortungsbereich
//! Siehe die Crate-Moduldoku (`crate`) für Format, die Quellenwahl gegenüber
//! `/sys/class/net/*/statistics/*`, die Kardinalitätsentscheidung und die
//! Begründung, warum `#[derive(harw_macros::SensorSource)]` diesen Fall nicht
//! trägt. Dieses Modul besitzt ausschließlich [`NetCountersSensor`] und seine
//! private Zeilenzuordnung ([`parse_one_interface_row`],
//! [`cap_and_sort_interfaces`]).
//!
//! # Nebenläufigkeit
//! [`NetCountersSensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self`.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — siehe [`NetCountersSensor::poll`] für die
//! vollständige Zuordnung.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Der relative Pfad der Quelle unterhalb der Wurzel des gebundenen
/// [`harw_dod_cap::ReadScope`] dieses Sensors.
///
/// # Description
/// In Produktion ist die Scope-Wurzel `/proc`, sodass sich daraus
/// `/proc/net/dev` ergibt — [`Capability::ReadProcNetDev::probe`] nennt
/// denselben Pfad informativ. In der Fixture-Harness ist die Scope-Wurzel
/// stattdessen `fixtures/<fall>/tree` bzw. `fixtures/malformed/<fall>`;
/// dieselbe relative Zusammensetzung liefert dort `.../tree/net/dev` bzw.
/// `.../<fall>/net/dev`.
const DEV_RELATIVE_PATH: &str = "net/dev";

/// Zahl der festen Kopfzeilen am Anfang von `/proc/net/dev`.
///
/// # Description
/// Seit Linux 2.2 stabiler, vom Kernel selbst erzeugter Text
/// (`net/core/net-procfs.c`, `dev_seq_show`): eine Rahmenzeile
/// (`Inter-|   Receive ...`) und eine Spaltenüberschriftenzeile
/// (` face |bytes ...`). [`NetCountersSensor::poll`] überspringt genau diese
/// Zahl an Zeilen **positionsbasiert**, nicht über Inhaltserkennung (siehe
/// `crate`-Moduldoku, Abschnitt „Das `/proc/net/dev`-Format"): fehlt eine der
/// beiden Kopfzeilen, verschiebt sich die Zuordnung, und die dadurch
/// fälschlich als Datenzeile gelesene Kopfzeile — oder das vollständige
/// Fehlen jeder verbleibenden Datenzeile — löst
/// [`SensorError::MalformedSource`] aus (siehe
/// `fixtures/malformed/missing-header`).
const HEADER_LINE_COUNT: usize = 2;

/// Mindestzahl numerischer Felder nach dem Doppelpunkt einer Schnittstellen-
/// zeile, unterhalb derer die Zeile als fehlerhaft gilt.
///
/// # Description
/// Der höchste in [`FIELD_METRICS`] referenzierte Spaltenindex ist `11`
/// (`tx_drop`, die vierte Sendespalte) — eine Zeile muss also mindestens
/// zwölf numerische Felder tragen, damit alle acht gemeldeten Zähler
/// berechenbar sind. Die vollständige `/proc/net/dev`-Zeile trägt seit
/// Jahrzehnten sechzehn Felder; diese Untergrenze ist bewusst kleiner
/// gewählt, damit ein hypothetischer, älterer oder abweichender Kernel mit
/// weniger (aber mindestens den hier gebrauchten) Spalten nicht grundlos
/// scheitert — symmetrisch zur Großzügigkeit gegenüber *zusätzlichen*
/// Spalten (siehe [`crate`]-Moduldoku).
const MIN_COLUMNS: usize = 12;

/// Die acht gemeldeten Zähler, als `(Metrikname, Spaltenindex)`-Paare.
///
/// # Description
/// Der Spaltenindex zählt ab `0` innerhalb der numerischen Felder einer
/// Schnittstellenzeile (nach Abtrennung des Namens, siehe
/// [`parse_one_interface_row`]): `0..=7` sind die acht Empfangsspalten
/// (`bytes packets errs drop fifo frame compressed multicast`), `8..=15`
/// die acht Sendespalten (`bytes packets errs drop fifo colls carrier
/// compressed`). Diese Crate meldet nur die vier operativ aussagekräftigsten
/// Spalten je Richtung (siehe `crate`-Moduldoku, Abschnitt „Gemeldete
/// Zähler") — `fifo`, `frame`/`colls`, `compressed` und `multicast`/`carrier`
/// bleiben unberücksichtigt.
const FIELD_METRICS: [(&str, usize); 8] = [
    ("rx_bytes", 0),
    ("rx_packets", 1),
    ("rx_errs", 2),
    ("rx_drop", 3),
    ("tx_bytes", 8),
    ("tx_packets", 9),
    ("tx_errs", 10),
    ("tx_drop", 11),
];

/// Die deklarierte Obergrenze der je Poll berücksichtigten Schnittstellen.
///
/// # Description
/// Schutz gegen die auf einem Container-Host mit vielen `veth`-Paaren
/// unbegrenzte Schnittstellenzahl (siehe `crate`-Moduldoku, Abschnitt
/// „Kardinalität"). Ein gewöhnlicher Host trägt selten mehr als eine
/// Handvoll Schnittstellen (`lo` plus einige physische/virtuelle Adapter);
/// dieser Wert deckt harte, aber noch gewöhnliche Fälle (mehrere physische
/// NICs, Bridges, VPN-Tunnel, Bonding-Mitglieder) großzügig ab, ohne
/// unbegrenzt zu wachsen. Schnittstellen über diese Zahl hinaus werden nach
/// Sortierung nach Namen verworfen (siehe [`cap_and_sort_interfaces`]) —
/// still, ohne Fehler: das ist eine Schutzmaßnahme, keine fehlerhafte
/// Quelle.
const MAX_INTERFACES: usize = 16;

/// Ersatzlabel, falls die Sanitisierung eines Schnittstellennamens ein leeres
/// Ergebnis liefern würde (praktisch unerreichbar bei gültigen
/// Kernel-Schnittstellennamen, aber total statt `unwrap`).
const FALLBACK_IFACE_LABEL: &str = "iface";

/// Obergrenze der Zeichen, die aus einem Schnittstellennamen in den
/// Metriknamen übernommen werden.
///
/// # Description
/// Linux begrenzt Schnittstellennamen ohnehin auf `IFNAMSIZ - 1 = 15` Byte;
/// diese Grenze ist trotzdem eigenständig gesetzt (nicht von `IFNAMSIZ`
/// abgeleitet), als Schutz gegen adversariell erzeugte Fixture-Bäume, deren
/// `/proc/net/dev`-Inhalt kein echter Kernel geschrieben hat (siehe
/// `fixtures/adversarial`-Konvention in `harw-dod-fixtures`).
const MAX_LABEL_LEN: usize = 32;

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll.
///
/// # Description
/// `FIELD_METRICS.len()` Zähler je Schnittstelle, höchstens
/// [`MAX_INTERFACES`] Schnittstellen je Poll (siehe `crate`-Moduldoku,
/// Abschnitt „Kardinalität"). Dieser Wert ist der Fixture-seitige Spiegel,
/// den [`harw_dod_fixtures::sensor_suite!`] als `max_cardinality` erwartet.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_netcounters::sensor::MAX_CARDINALITY, 128);
/// ```
pub const MAX_CARDINALITY: usize = FIELD_METRICS.len() * MAX_INTERFACES;

/// Liest die Netzschnittstellen-Zähler aus `/proc/net/dev` — genau eine
/// Quelle, genau eine Fähigkeit ([`Capability::ReadProcNetDev`]).
///
/// # Description
/// Siehe die Crate-Moduldoku für das Dateiformat, die Quellenwahl und die
/// Kardinalitätsentscheidung. Konstruiert ausschließlich über
/// [`From<SensorHandle<Bound>>`], die Konvention, die
/// [`harw_dod_fixtures::sensor_suite!`] von jedem Sensor-Typ verlangt.
#[derive(Debug)]
pub struct NetCountersSensor {
    handle: SensorHandle<Bound>,
}

impl NetCountersSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration für das CI-Privilegienbudget-Gate,
    /// nachgebildet aus dem, was `#[derive(harw_macros::SensorSource)]` an
    /// derselben Stelle erzeugen würde (siehe `crate`-Moduldoku, Abschnitt
    /// „Quellenwahl").
    ///
    /// # Returns
    /// [`Capability::ReadProcNetDev`], die einzige Fähigkeit dieses Sensors.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_netcounters::NetCountersSensor;
    ///
    /// assert_eq!(NetCountersSensor::CAPABILITY, Capability::ReadProcNetDev);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadProcNetDev;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Ein fester Bezeichner für die Sensor*art* — unabhängig von der
    /// `harw_types::SensorId`, die eine konkrete Instanz über
    /// [`SensorHandle::new`] erhält.
    ///
    /// # Returns
    /// `"netcounters"`.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netcounters::NetCountersSensor;
    ///
    /// assert_eq!(NetCountersSensor::SENSOR_ID, "netcounters");
    /// ```
    pub const SENSOR_ID: &'static str = "netcounters";
}

impl From<SensorHandle<Bound>> for NetCountersSensor {
    /// Baut einen `NetCountersSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige Konstruktionskonvention, die
    /// [`harw_dod_fixtures::sensor_suite!`] von jedem Sensor-Typ verlangt
    /// (siehe dessen Moduldoku, Abschnitt „Voraussetzung an `sensor`").
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, dessen
    ///   Fähigkeit üblicherweise [`NetCountersSensor::CAPABILITY`] ist (nicht
    ///   von diesem Konstruktor erzwungen — die Fähigkeit steht bereits im
    ///   Griff, bevor er hier ankommt).
    ///
    /// # Returns
    /// Einen `NetCountersSensor`, der über `handle.scope()` liest.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_netcounters::NetCountersSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
    /// let handle = SensorHandle::new(SensorId::from_str("netcounters-0"), Capability::ReadProcNetDev)
    ///     .bind(scope);
    /// let _sensor = NetCountersSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for NetCountersSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei der Konstruktion übergebenen
    /// `SensorHandle<Bound>`.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest `/proc/net/dev` einmal aus und meldet acht kumulative Zähler je
    /// sichtbarer Schnittstelle, bis zu [`MAX_INTERFACES`] Schnittstellen.
    ///
    /// # Description
    /// Baut den Quellpfad als `scope.roots().next().join("net/dev")` (siehe
    /// [`DEV_RELATIVE_PATH`]) und liest ihn über
    /// [`harw_dod_readfs::read_line_fields`] — nie direktes `std::fs`.
    /// Überspringt die ersten [`HEADER_LINE_COUNT`] Zeilen positionsbasiert
    /// (siehe [`HEADER_LINE_COUNT`]-Doku für die Begründung), parst jede
    /// verbleibende Zeile über [`parse_one_interface_row`], sortiert das
    /// Ergebnis nach Schnittstellenname und kürzt es auf [`MAX_INTERFACES`]
    /// Einträge (siehe [`cap_and_sort_interfaces`]). Für jede verbleibende
    /// Schnittstelle entstehen [`FIELD_METRICS`]`.len()` `HostSample`s,
    /// deren `metric` das sanitisierte Schnittstellenlabel als Suffix trägt
    /// (siehe [`sanitize_label`]). `now` wird unverändert in jedes
    /// `HostSample` durchgereicht; die Systemuhr wird an keiner Stelle
    /// gelesen.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit für
    ///   `HostSample::observed_at`.
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit bis zu [`MAX_CARDINALITY`] Samples und
    /// keinen Events (`/proc/net/dev` erzeugt keine Sicherheitsereignisse).
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: der Lesebereich hat keine
    ///   Wurzel, oder `net/dev` existiert unterhalb der Wurzel nicht (der
    ///   Fall eines leeren oder unpassenden Bereichs — siehe die
    ///   Scope-Dichtheit-Prüfung der Fixture-Harness).
    /// - [`SensorError::MalformedSource`]: nach Abzug der
    ///   [`HEADER_LINE_COUNT`] Kopfzeilen bleibt keine Datenzeile übrig, eine
    ///   verbleibende Zeile trägt keinen Doppelpunkt, ihr Schnittstellenname
    ///   ist leer, sie hat weniger als [`MIN_COLUMNS`] numerische Felder,
    ///   oder eines ihrer Felder ist keine gültige `u64`-Zahl. Bricht den
    ///   gesamten Abruf ab, statt eine defekte Zeile stillschweigend
    ///   auszulassen — analog zu `harw-dod-thermal`s Umgang mit einer
    ///   fehlenden `temp`-Datei: ein Konsument, der Zählerwerte für
    ///   Schwellwertentscheidungen nutzt, soll eine unvollständige
    ///   Schnittstellenmenge nie mit „alles in Ordnung" verwechseln können.
    ///   **Der nicht passende Zeileninhalt erscheint in keinem dieser Fälle
    ///   in der Fehlermeldung.**
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
        let path = root.join(DEV_RELATIVE_PATH);

        let rows = match harw_dod_readfs::read_line_fields(scope, &path) {
            Ok(rows) => rows,
            Err(ReadFsError::Scope(SensorError::Io(io_err)))
                if io_err.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(SensorError::SourceUnavailable);
            }
            Err(ReadFsError::Scope(inner)) => return Err(inner),
            Err(ReadFsError::TooLarge { .. }) => return Err(SensorError::MalformedSource),
            Err(ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. }) => {
                // read_line_fields ruft nie glob() auf; unerreichbar, aber
                // erschöpfend abgedeckt, damit eine künftige ReadFsError-
                // Variante hier nicht still verworfen wird.
                return Err(SensorError::MalformedSource);
            }
        };

        let data_rows: &[Vec<String>] = rows.get(HEADER_LINE_COUNT..).unwrap_or(&[]);
        if data_rows.is_empty() {
            return Err(SensorError::MalformedSource);
        }

        let parsed = data_rows
            .iter()
            .map(|fields| parse_one_interface_row(fields))
            .collect::<Result<Vec<_>, _>>()?;
        let kept = cap_and_sort_interfaces(parsed);

        let mut samples = Vec::with_capacity(kept.len() * FIELD_METRICS.len());
        for (name, values) in &kept {
            let label = sanitize_label(name);
            for (metric_name, idx) in FIELD_METRICS {
                let value = values.get(idx).copied().unwrap_or(0);
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{metric_name}_{label}")),
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

/// Parst eine einzelne Schnittstellenzeile von `/proc/net/dev`.
///
/// # Description
/// `fields` ist das Ergebnis von [`harw_dod_readfs::read_line_fields`] für
/// eine Zeile: ihre leerraumgetrennten Felder in Datei- und
/// Spaltenreihenfolge. Das erste Feld trägt immer den Schnittstellennamen
/// mit unmittelbar anklebendem Doppelpunkt (`"eth0:"`), weil der Kernel ohne
/// Leerzeichen dazwischen formatiert (siehe `crate`-Moduldoku, Abschnitt
/// „Das `/proc/net/dev`-Format"). Bei einem langen Schnittstellennamen
/// **und** einem ersten Zählerwert, der die volle Spaltenbreite ausfüllt,
/// kann sogar dieser erste Wert noch am selben Feld kleben (`"eth0:1234"`
/// statt `"eth0:" "1234"`); diese Funktion behandelt beide Formen identisch,
/// indem sie das erste Feld am **ersten** Doppelpunkt trennt und einen
/// nicht-leeren Rest als ersten numerischen Wert voranstellt.
///
/// # Arguments
/// - `fields` (`&[String]`): die Felder einer einzelnen Zeile, wie von
///   [`harw_dod_readfs::read_line_fields`] geliefert.
///
/// # Returns
/// Den Schnittstellennamen (roh, vor der Sanitisierung — siehe
/// [`sanitize_label`]) und mindestens [`MIN_COLUMNS`] geparste `u64`-Werte in
/// Spaltenreihenfolge.
///
/// # Errors
/// [`SensorError::MalformedSource`], wenn `fields` leer ist, das erste Feld
/// keinen Doppelpunkt trägt, der Name vor dem Doppelpunkt leer ist, weniger
/// als [`MIN_COLUMNS`] numerische Felder vorhanden sind, oder eines dieser
/// Felder keine gültige `u64`-Zahl ist. Der nicht passende Zeileninhalt
/// erscheint nie in der Fehlermeldung — [`SensorError::MalformedSource`] ist
/// inhaltsfrei.
fn parse_one_interface_row(fields: &[String]) -> Result<(String, Vec<u64>), SensorError> {
    let first = fields.first().ok_or(SensorError::MalformedSource)?;
    let (name, glued_suffix) = first.split_once(':').ok_or(SensorError::MalformedSource)?;
    if name.is_empty() {
        return Err(SensorError::MalformedSource);
    }

    let mut values: Vec<u64> = Vec::with_capacity(fields.len());
    if !glued_suffix.is_empty() {
        let value = glued_suffix
            .trim()
            .parse::<u64>()
            .map_err(|_| SensorError::MalformedSource)?;
        values.push(value);
    }
    for raw in &fields[1..] {
        let value = raw
            .trim()
            .parse::<u64>()
            .map_err(|_| SensorError::MalformedSource)?;
        values.push(value);
    }

    if values.len() < MIN_COLUMNS {
        return Err(SensorError::MalformedSource);
    }

    Ok((name.to_owned(), values))
}

/// Sortiert geparste Schnittstellen nach Namen und kürzt sie auf
/// [`MAX_INTERFACES`] Einträge.
///
/// # Description
/// Sortierung vor der Kürzung macht das Ergebnis unabhängig von der
/// Zeilenreihenfolge in `/proc/net/dev` — deterministisch bei gleichem
/// Fixture-Inhalt (siehe `crate`-Moduldoku, Abschnitt „Kardinalität") und
/// vorhersagbar für einen Betreiber, der weiß, welche Schnittstellen bei
/// einer Überschreitung der Grenze erhalten bleiben (die alphabetisch
/// ersten), statt von einer zufälligen Kernel-internen Registrierungs-
/// reihenfolge abzuhängen.
///
/// # Arguments
/// - `parsed` (`Vec<(String, Vec<u64>)>`): alle in einem Poll erfolgreich
///   geparsten Schnittstellen, in beliebiger Reihenfolge.
///
/// # Returns
/// Höchstens [`MAX_INTERFACES`] Einträge aus `parsed`, aufsteigend nach dem
/// Schnittstellennamen sortiert.
///
/// # Errors
/// Keine — eine totale Funktion.
fn cap_and_sort_interfaces(mut parsed: Vec<(String, Vec<u64>)>) -> Vec<(String, Vec<u64>)> {
    parsed.sort_by(|a, b| a.0.cmp(&b.0));
    parsed.truncate(MAX_INTERFACES);
    parsed
}

/// Reduziert einen Schnittstellennamen auf sichere Metriknamen-Zeichen.
///
/// # Description
/// Analog zu `harw-dod-thermal`s Zonenlabel-Sanitisierung: alle
/// ASCII-alphanumerischen Zeichen werden kleingeschrieben übernommen, jedes
/// andere Zeichen (insbesondere `.` — siehe `crate`-Moduldoku, Abschnitt „Was
/// dieser Sensor ausdrücklich NICHT meldet") wird durch `_` ersetzt, das
/// Ergebnis auf [`MAX_LABEL_LEN`] Zeichen gekürzt. Das ist die einzige Stelle,
/// an der ein aus der Quelle gelesener Text in einen Metriknamen einfließt —
/// entscheidend dafür, dass ein adressartiger Schnittstellenname (z. B. eine
/// VLAN-Subschnittstelle `1.2.3.4`) nie unverändert im emittierten Ergebnis
/// erscheint.
///
/// # Arguments
/// - `raw` (`&str`): der rohe, aus `/proc/net/dev` gelesene
///   Schnittstellenname, vor jeder Bereinigung.
///
/// # Returns
/// Ein sanitisiertes Label aus `[a-z0-9_]`, nie leer (siehe
/// [`FALLBACK_IFACE_LABEL`]).
///
/// # Errors
/// Keine — eine totale Funktion.
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
        FALLBACK_IFACE_LABEL.to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_dod_cap::{Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::Sensor;
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::{
        cap_and_sort_interfaces, parse_one_interface_row, sanitize_label, NetCountersSensor,
        FIELD_METRICS, MAX_CARDINALITY, MAX_INTERFACES,
    };

    /// Das `fixtures/`-Wurzelverzeichnis dieser Crate.
    fn fixtures_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    /// Baut einen `NetCountersSensor`, dessen `ReadScope` genau `root`
    /// umfasst.
    fn build_sensor(root: PathBuf) -> NetCountersSensor {
        let scope = ReadScope::from_roots([root]);
        let handle = SensorHandle::new(
            SensorId::from_str("netcounters-test"),
            Capability::ReadProcNetDev,
        )
        .bind(scope);
        NetCountersSensor::from(handle)
    }

    fn fields_from(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    // ---- Konstanten ----

    #[test]
    fn test_capability_and_sensor_id_constants() {
        assert_eq!(NetCountersSensor::CAPABILITY, Capability::ReadProcNetDev);
        assert_eq!(NetCountersSensor::SENSOR_ID, "netcounters");
    }

    #[test]
    fn test_max_cardinality_matches_field_metrics_times_max_interfaces() {
        assert_eq!(MAX_CARDINALITY, FIELD_METRICS.len() * MAX_INTERFACES);
        assert_eq!(MAX_CARDINALITY, 128);
    }

    // ---- parse_one_interface_row ----

    #[test]
    fn test_parse_one_interface_row_maps_middle_column_not_just_the_first() {
        let fields = fields_from(
            "eth0: 500000 4000 1 2 0 0 0 3 600000 4500 0 1 0 0 0 0",
        );
        let (name, values) =
            parse_one_interface_row(&fields).expect("wohlgeformte Zeile muss geparst werden");

        assert_eq!(name, "eth0");
        // tx_bytes (Index 8) ist ein Wert aus der Mitte der Zeile, nicht der
        // erste — nur so belegt der Test, dass die Spaltenzuordnung wirklich
        // stimmt und nicht zufällig, weil Position 0 zufällig passt.
        assert_eq!(values[8], 600_000);
        assert_eq!(values[0], 500_000);
    }

    #[test]
    fn test_parse_one_interface_row_splits_colon_attached_to_name() {
        // Der Normalfall: der Doppelpunkt klebt am Namen, aber ein
        // Leerzeichen trennt ihn vom ersten Zahlenwert.
        let fields = fields_from("lo: 733258 5340 0 0 0 0 0 0 733258 5340 0 0 0 0 0 0");
        let (name, values) =
            parse_one_interface_row(&fields).expect("wohlgeformte Zeile muss geparst werden");

        assert_eq!(name, "lo");
        assert_eq!(values.len(), 16);
        assert_eq!(values[0], 733_258);
    }

    #[test]
    fn test_parse_one_interface_row_splits_colon_glued_to_first_value() {
        // Der Grenzfall: ein langer Schnittstellenname plus ein erster
        // Zählerwert, der die volle Spaltenbreite ausfüllt, lässt den Kernel
        // ohne trennendes Leerzeichen formatieren — Name, Doppelpunkt und
        // erster Wert kommen als ein einziges Whitespace-Token an.
        let mut fields = vec!["veryverylongifname0:12345678".to_owned()];
        fields.extend(fields_from("4000 1 2 0 0 0 3 600000 4500 0 1 0 0 0 0"));

        let (name, values) =
            parse_one_interface_row(&fields).expect("glued Doppelpunkt muss getrennt werden");

        assert_eq!(name, "veryverylongifname0");
        assert_eq!(values[0], 12_345_678);
        assert_eq!(values[8], 600_000);
        assert_eq!(values.len(), 16);
    }

    #[test]
    fn test_parse_one_interface_row_accepts_extra_trailing_columns() {
        // Der wichtigste Robustheitsfall: ein künftiger Kernel hängt weitere
        // Spalten an. Der Parser darf daran nicht scheitern.
        let fields = fields_from(
            "eth0: 500000 4000 1 2 0 0 0 3 600000 4500 0 1 0 0 0 0 999 888",
        );
        let (_, values) =
            parse_one_interface_row(&fields).expect("zusätzliche Spalten dürfen nicht scheitern");
        assert_eq!(values.len(), 18);
        assert_eq!(values[8], 600_000);
    }

    #[test]
    fn test_parse_one_interface_row_rejects_too_few_columns() {
        let fields = fields_from("lo: 1 2 3");
        let err = parse_one_interface_row(&fields)
            .expect_err("zu wenige Spalten müssen scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_one_interface_row_rejects_non_numeric_field_without_leaking_content() {
        let fields = fields_from("eth0: 1 not-a-number 3 4 0 0 0 0 5 6 0 0");
        let err = parse_one_interface_row(&fields)
            .expect_err("nicht-numerischer Wert muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
        let rendered = format!("{err}{err:?}");
        assert!(
            !rendered.contains("not-a-number"),
            "Fehlermeldung darf den gelesenen Inhalt nicht enthalten: {rendered}"
        );
    }

    #[test]
    fn test_parse_one_interface_row_rejects_missing_colon() {
        let fields = fields_from("eth0 1 2 3 4 5 6 7 8 9 10 11 12");
        let err = parse_one_interface_row(&fields)
            .expect_err("eine Zeile ohne Doppelpunkt muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_one_interface_row_rejects_empty_name() {
        let fields = fields_from(": 1 2 3 4 5 6 7 8 9 10 11 12");
        let err = parse_one_interface_row(&fields)
            .expect_err("ein leerer Schnittstellenname muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    // ---- cap_and_sort_interfaces ----

    #[test]
    fn test_cap_and_sort_interfaces_sorts_alphabetically_not_by_file_order() {
        // Datei-Reihenfolge ist absichtlich absteigend (z2, z1, a1), damit
        // ein Fehler, der nur die Datei-Reihenfolge kürzt statt vorher zu
        // sortieren, hier sichtbar würde.
        let parsed = vec![
            ("z2".to_owned(), vec![0u64; 16]),
            ("z1".to_owned(), vec![0u64; 16]),
            ("a1".to_owned(), vec![0u64; 16]),
        ];
        let kept = cap_and_sort_interfaces(parsed);
        let names: Vec<&str> = kept.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["a1", "z1", "z2"]);
    }

    #[test]
    fn test_cap_and_sort_interfaces_truncates_to_max_interfaces() {
        let parsed: Vec<(String, Vec<u64>)> = (0..(MAX_INTERFACES + 5))
            .map(|i| (format!("if{i:02}"), vec![0u64; 16]))
            .collect();
        let kept = cap_and_sort_interfaces(parsed);

        assert_eq!(kept.len(), MAX_INTERFACES);
        // Alphabetisch sind "if00".."if15" die ersten Sechzehn von "if00"
        // bis "if20" (zweistellig, gleiche Präfixlänge) — "if16" und höher
        // müssen fehlen.
        let names: Vec<&str> = kept.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"if00"));
        assert!(names.contains(&"if15"));
        assert!(!names.contains(&"if16"));
    }

    // ---- sanitize_label ----

    #[test]
    fn test_sanitize_label_lowercases_and_keeps_alnum() {
        assert_eq!(sanitize_label("Eth0"), "eth0");
    }

    #[test]
    fn test_sanitize_label_never_reproduces_dotted_quad_pattern() {
        // Linux erlaubt Punkte in Schnittstellennamen (VLAN-Subschnitt-
        // stellen wie "eth0.100"); ein zufällig adressartig benannter
        // Interface-Name darf trotzdem nie unverändert in den Metriknamen
        // durchsickern.
        let label = sanitize_label("1.2.3.4");
        assert!(!label.contains('.'));
        assert_eq!(label, "1_2_3_4");
    }

    #[test]
    fn test_sanitize_label_truncates_and_never_empty() {
        let long = "x".repeat(200);
        let label = sanitize_label(&long);
        assert!(label.len() <= 32);

        // Nur ein Eingabewert, der nach dem Trimmen keine Zeichen mehr
        // übrig lässt (leer oder ausschließlich Leerraum), löst das
        // Ersatzlabel aus: jedes andere Zeichen wird durch `_` ersetzt,
        // nicht entfernt.
        let empty = sanitize_label("   ");
        assert_eq!(empty, "iface");
        let also_empty = sanitize_label("");
        assert_eq!(also_empty, "iface");

        // Nicht-alphanumerische, aber nicht-leere Eingaben werden zu
        // Unterstrichen, nicht zum Ersatzlabel.
        let dots = sanitize_label("...");
        assert_eq!(dots, "___");
    }

    // ---- Fixture-gestützte Prüfungen ----

    #[test]
    fn test_multi_interface_fixture_orders_samples_alphabetically_by_interface() {
        let sensor = build_sensor(fixtures_root().join("multi-interface/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("multi-interface Fixture muss erfolgreich pollen");

        assert_eq!(reading.samples.len(), 16);
        assert!(reading.samples[0].metric.ends_with("_eth0"));
        assert!(reading.samples[8].metric.ends_with("_lo"));
    }

    #[test]
    fn test_many_interfaces_fixture_enforces_cardinality_cap() {
        let sensor = build_sensor(fixtures_root().join("many-interfaces/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("many-interfaces Fixture muss erfolgreich pollen");

        assert_eq!(reading.samples.len(), MAX_CARDINALITY);
        assert!(reading.samples.iter().any(|s| s.metric.ends_with("_if00")));
        assert!(!reading.samples.iter().any(|s| s.metric.ends_with("_if16")));
    }

    #[test]
    fn test_poll_is_deterministic_for_same_now() {
        let sensor = build_sensor(fixtures_root().join("multi-interface/tree"));
        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("erster Poll muss gelingen");
        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("zweiter Poll muss gelingen");
        assert_eq!(first, second);
    }

    /// Der wichtigste Test dieser Crate: kein serialisiertes Feld darf eine
    /// Adresse oder einen Port tragen (siehe `crate`-Moduldoku, Abschnitt
    /// „Was dieser Sensor ausdrücklich NICHT meldet").
    #[test]
    fn test_serialized_reading_never_contains_address_or_port_pattern() {
        let sensor = build_sensor(fixtures_root().join("multi-interface/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("multi-interface Fixture muss erfolgreich pollen");

        let json = serde_json::to_string(&reading.samples).expect("HostSample serialisiert");
        assert!(
            !contains_ipv4_like_pattern(&json),
            "IPv4-artiges Adressmuster im serialisierten Ergebnis gefunden: {json}"
        );

        for sample in &reading.samples {
            assert!(
                !sample.metric.contains(':'),
                "ein Metrikname darf keinen Doppelpunkt enthalten (möglicher Port-/Adress- \
                 Trenner): {}",
                sample.metric
            );
            assert!(
                !sample.metric.contains('.'),
                "ein Metrikname darf keinen Punkt enthalten (möglicher Adress-Trenner): {}",
                sample.metric
            );
        }
    }

    /// Sucht `haystack` nach einem IPv4-artigen Muster ab: vier durch `.`
    /// getrennte Gruppen von je ein bis drei Ziffern, deren Wert `u8` nicht
    /// überschreitet. Testhelfer, kein Bestandteil der Sensor-Logik.
    fn contains_ipv4_like_pattern(haystack: &str) -> bool {
        haystack
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .any(|token| {
                let parts: Vec<&str> = token.split('.').collect();
                parts.len() == 4
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
            })
    }
}
