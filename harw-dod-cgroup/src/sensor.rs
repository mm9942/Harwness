//! `CgroupSensor` — handgeschriebene `Sensor`-Implementierung für die
//! cgroup-v2-Kinder der Bereichswurzel.
//!
//! # Verantwortungsbereich
//! Siehe die Crate-Moduldoku (`crate`) für die Quellenwahl (vier
//! Kontrolldateien), die Behandlung von `max`, die
//! Fehlend-vs-fehlerhaft-Unterscheidung, die Kardinalitätsgrenze und die
//! Namensbereinigung. Dieses Modul besitzt ausschließlich [`CgroupSensor`]
//! und seine private Lese-/Sanitisierungslogik.
//!
//! # Nebenläufigkeit
//! [`CgroupSensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self`.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — siehe [`CgroupSensor::poll`] für die
//! vollständige Zuordnung.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use harw_types::SensorId;
use jiff::Timestamp;

/// Suffix-Glob-Muster für die Kind-cgroups, relativ zur ersten
/// Bereichswurzel — genau eine Ebene, kein `**` (siehe `crate`-Moduldoku,
/// Abschnitt „Welche Ebene").
const CGROUP_DIR_GLOB_SUFFIX: &str = "*";

/// Dateiname: aktueller Speicherverbrauch in Byte.
const MEMORY_CURRENT_FILE: &str = "memory.current";
/// Dateiname: konfiguriertes Speicherlimit in Byte, oder das Literal `max`.
const MEMORY_MAX_FILE: &str = "memory.max";
/// Dateiname: aktuelle Zahl der Prozesse/Threads dieser cgroup.
const PIDS_CURRENT_FILE: &str = "pids.current";
/// Dateiname: schlüsselwertartige CPU-Zeitstatistik dieser cgroup.
const CPU_STAT_FILE: &str = "cpu.stat";
/// Schlüssel in `cpu.stat`, dessen Wert diese Crate meldet — kumulierte
/// CPU-Zeit in Mikrosekunden seit Erzeugung der cgroup.
const CPU_STAT_USAGE_KEY: &str = "usage_usec";
/// Trennzeichen zwischen Schlüssel und Wert in `cpu.stat`-Zeilen
/// (`"usage_usec 12345"`).
const CPU_STAT_SEPARATOR: char = ' ';

/// Der getrimmte Dateiinhalt, der „kein Speicherlimit gesetzt" bedeutet
/// (siehe `crate`-Moduldoku, Entscheidung 2).
const MEMORY_MAX_UNLIMITED_LITERAL: &str = "max";

/// Metrikname: aktueller Speicherverbrauch in Byte.
const METRIC_MEMORY_CURRENT: &str = "memory_current_bytes";
/// Metrikname: konfiguriertes Speicherlimit in Byte (`u64::MAX`-Stellvertreter
/// bei fehlendem Limit, siehe `crate`-Moduldoku, Entscheidung 2).
const METRIC_MEMORY_MAX: &str = "memory_max_bytes";
/// Metrikname: aktuelle Zahl der Prozesse/Threads.
const METRIC_PIDS_CURRENT: &str = "pids_current";
/// Metrikname: kumulierte CPU-Zeit in Mikrosekunden.
const METRIC_CPU_USAGE_USEC: &str = "cpu_usage_usec";

/// Zahl der von diesem Sensor gemeldeten Metriken je cgroup (siehe
/// `crate`-Moduldoku, Abschnitt „Welche Kontrolldateien").
const FIELD_COUNT: usize = 4;

/// Die deklarierte Obergrenze der je Poll berücksichtigten cgroups.
///
/// # Description
/// Schutz gegen die auf einem Host mit vielen Containern unbegrenzte
/// Zahl an Kind-cgroups (siehe `crate`-Moduldoku, Abschnitt
/// „Kardinalität"). cgroups über diese Zahl hinaus werden nach Sortierung
/// nach Namen verworfen (siehe [`cap_and_sort_cgroups`]) — still, ohne
/// Fehler: das ist eine Schutzmaßnahme, keine fehlerhafte Quelle.
pub const MAX_CGROUPS: usize = 16;

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll.
///
/// # Description
/// [`MAX_CGROUPS`] cgroups mit je bis zu [`FIELD_COUNT`] Metriken. Der
/// Fixture-seitige Spiegel, den `harw_dod_fixtures::sensor_suite!` als
/// `max_cardinality` erwartet.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_cgroup::sensor::MAX_CARDINALITY, 64);
/// ```
pub const MAX_CARDINALITY: usize = MAX_CGROUPS * FIELD_COUNT;

/// Obergrenze der Zeichen, die aus einem cgroup-Namen in den Metriknamen
/// übernommen werden — Schutz gegen unbegrenztes Wachstum durch einen
/// ungewöhnlich langen, adversariell erzeugten Namen.
const MAX_LABEL_LEN: usize = 64;

/// Ersatzlabel, falls die Sanitisierung eines cgroup-Namens ein leeres
/// Ergebnis liefern würde (praktisch unerreichbar bei einem echten
/// Verzeichnisnamen, aber total statt `unwrap`).
const FALLBACK_LABEL: &str = "cgroup";

/// Liest vier Zähler je sichtbarer Kind-cgroup unterhalb der
/// Bereichswurzel — genau eine Quelle, genau eine Fähigkeit
/// ([`Capability::ReadCgroupV2`]).
///
/// # Description
/// Siehe die Crate-Moduldoku für das cgroup-v2-Format, die Kontrolldatei-
/// Auswahl, die Behandlung von `max` und die Kardinalitätsgrenze.
/// Konstruiert ausschließlich über [`From<SensorHandle<Bound>>`], die
/// Konvention, die `harw_dod_fixtures::sensor_suite!` von jedem
/// Sensor-Typ verlangt.
#[derive(Debug)]
pub struct CgroupSensor {
    handle: SensorHandle<Bound>,
}

impl CgroupSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration, analog zur gleichnamigen Konstante, die
    /// `#[derive(harw_macros::SensorSource)]` für andere Sensor-Crates
    /// erzeugt (siehe `crate`-Moduldoku, Abschnitt „Warum kein
    /// `#[derive(SensorSource)]`").
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_cgroup::CgroupSensor;
    ///
    /// assert_eq!(CgroupSensor::CAPABILITY, Capability::ReadCgroupV2);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadCgroupV2;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Ein fester Bezeichner für die Sensor*art* — unabhängig von der
    /// `harw_types::SensorId`, die eine konkrete Instanz über
    /// [`SensorHandle::new`] erhält.
    ///
    /// # Examples
    /// ```rust
    /// assert_eq!(harw_dod_cgroup::CgroupSensor::SENSOR_ID, "cgroup");
    /// ```
    pub const SENSOR_ID: &'static str = "cgroup";
}

impl From<SensorHandle<Bound>> for CgroupSensor {
    /// Baut einen `CgroupSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige Konstruktionskonvention, die
    /// `harw_dod_fixtures::sensor_suite!` von jedem Sensor-Typ verlangt.
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, dessen
    ///   Fähigkeit üblicherweise [`CgroupSensor::CAPABILITY`] ist (nicht von
    ///   diesem Konstruktor erzwungen — die Fähigkeit steht bereits im
    ///   Griff, bevor er hier ankommt).
    ///
    /// # Returns
    /// Einen `CgroupSensor`, der über `handle.scope()` liest.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_cgroup::CgroupSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/sys/fs/cgroup")]);
    /// let handle = SensorHandle::new(SensorId::from_str("cgroup-0"), Capability::ReadCgroupV2)
    ///     .bind(scope);
    /// let _sensor = CgroupSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for CgroupSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei [`CgroupSensor::from`] übergebenen
    /// `SensorHandle<Bound>`.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest alle sichtbaren Kind-cgroups der Bereichswurzel einmal aus.
    ///
    /// # Description
    /// Ermittelt zuerst die Kind-Verzeichnisse direkt unterhalb der ersten
    /// Bereichswurzel (`*`, siehe `crate`-Moduldoku, Abschnitt „Welche
    /// Ebene"), sortiert sie nach Namen und kürzt auf [`MAX_CGROUPS`]
    /// (siehe [`cap_and_sort_cgroups`]). Liest je verbleibender cgroup bis zu
    /// vier Kontrolldateien (siehe `crate`-Moduldoku, Abschnitt „Welche
    /// Kontrolldateien") und meldet jede erfolgreich gelesene als ein
    /// `HostSample`, dessen `metric` den festen Namen der Kennzahl plus das
    /// sanitisierte cgroup-Label trägt (siehe [`sanitize_label`]). Fehlt eine
    /// Kontrolldatei (Controller nicht aktiviert, oder der Kandidat ist gar
    /// kein Verzeichnis), wird nur diese eine Metrik ausgelassen — siehe
    /// `crate`-Moduldoku, Entscheidung 3.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit für
    ///   `HostSample::observed_at`.
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit bis zu [`MAX_CARDINALITY`] Samples und
    /// keinen Events (cgroup-v2-Kontrolldateien erzeugen keine
    /// Sicherheitsereignisse). Kann leer sein, wenn jede gefundene cgroup
    /// keine der vier Kontrolldateien trägt (Controller nirgends aktiviert)
    /// — das ist kein Fehler, siehe `crate`-Moduldoku, Entscheidung 3.
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: der Lesebereich hat keine
    ///   Wurzel, oder die Bereichswurzel enthält **keinen einzigen**
    ///   sichtbaren Eintrag. Jeder von `systemd` verwaltete Linux-Host trägt
    ///   unter `/sys/fs/cgroup` mindestens `system.slice`, `user.slice` und
    ///   `init.scope` — eine vollständig leere Bereichswurzel ist deshalb,
    ///   wie bei `harw-dod-thermal` und `harw-dod-cpu`, ein Anzeichen für
    ///   einen falsch konfigurierten oder falsch gerichteten Bereich, nicht
    ///   der gesunde Normalfall (anders als bei `harw-dod-gpu`, wo die
    ///   *Hardware* selbst auf vielen Hosts fehlt).
    /// - [`SensorError::MalformedSource`]: eine gefundene cgroup trägt eine
    ///   der vier Kontrolldateien, aber mit unerwartetem Inhalt (leer, nicht
    ///   numerisch, `cpu.stat` ohne `usage_usec`-Zeile) — siehe `crate`-
    ///   Moduldoku, Entscheidung 3. Bricht den gesamten Abruf ab.
    /// - [`SensorError::OutsideScope`] / [`SensorError::Io`]: unverändert
    ///   durchgereicht, falls eine Lesefunktion einen Bereichsverstoß meldet.
    ///   Ein gewöhnlicher „Datei nicht gefunden"/„kein Verzeichnis"-Fehler
    ///   führt dagegen zu `None` für die betroffene Metrik, nicht zu diesem
    ///   Fehler (siehe [`read_optional_u64`]).
    ///
    /// # Examples
    /// Siehe die Crate-Moduldoku für ein vollständiges Beispiel.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let candidates = cgroup_dirs(scope)?;
        if candidates.is_empty() {
            return Err(SensorError::SourceUnavailable);
        }

        let kept = cap_and_sort_cgroups(candidates);

        let mut samples = Vec::with_capacity(kept.len() * FIELD_COUNT);
        for dir in &kept {
            let label = sanitize_label(cgroup_name(dir));

            if let Some(value) = read_optional_u64(scope, &dir.join(MEMORY_CURRENT_FILE))? {
                samples.push(build_sample(
                    self.handle.id(),
                    now,
                    METRIC_MEMORY_CURRENT,
                    &label,
                    value as f64,
                ));
            }
            if let Some(value) = read_memory_max(scope, &dir.join(MEMORY_MAX_FILE))? {
                samples.push(build_sample(
                    self.handle.id(),
                    now,
                    METRIC_MEMORY_MAX,
                    &label,
                    value as f64,
                ));
            }
            if let Some(value) = read_optional_u64(scope, &dir.join(PIDS_CURRENT_FILE))? {
                samples.push(build_sample(
                    self.handle.id(),
                    now,
                    METRIC_PIDS_CURRENT,
                    &label,
                    value as f64,
                ));
            }
            if let Some(value) = read_cpu_usage_usec(scope, &dir.join(CPU_STAT_FILE))? {
                samples.push(build_sample(
                    self.handle.id(),
                    now,
                    METRIC_CPU_USAGE_USEC,
                    &label,
                    value as f64,
                ));
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
// vollen Pfad fest zu verdrahten (K42 — siehe `crate`-Moduldoku und
// `harw-dod-thermal`s `relative_pattern`, von dem dies eine Kopie ist:
// `harw_dod_readfs::glob::glob` sucht laut eigener Moduldoku immer ab `/`
// und nutzt den `ReadScope` nur als nachträglichen Filter; ein zur
// Kompilierzeit fest verdrahtetes Muster fände deshalb in einer
// Fixture-Prüfung, deren Bereichswurzel ein `fixtures/<fall>/tree`-
// Verzeichnis ist, niemals etwas. `None`, wenn `root` nicht absolut ist —
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

// Findet alle Einträge direkt unterhalb der ersten Bereichswurzel von
// `scope`, sortiert (siehe `glob::glob`s eigene Garantie). Kann sowohl echte
// Kind-cgroups (Verzeichnisse) als auch die Kontrolldateien der
// Bereichswurzel selbst enthalten (beide passen gleichermaßen auf `*`) — ein
// solcher Kandidat liefert beim späteren Lesen für jede der vier
// Kontrolldateien `ENOTDIR` und damit `None`, erzeugt also nie einen
// `HostSample` (siehe `crate`-Moduldoku, Abschnitt „Welche Ebene"). Ein
// `scope` ohne Wurzeln oder mit nicht-absoluter Wurzel liefert eine leere
// Liste statt eines Fehlers — `poll` bildet das auf
// `SensorError::SourceUnavailable` ab.
fn cgroup_dirs(scope: &ReadScope) -> Result<Vec<PathBuf>, SensorError> {
    let Some(root) = scope.roots().next() else {
        return Ok(Vec::new());
    };
    let Some(pattern) = relative_pattern(root, CGROUP_DIR_GLOB_SUFFIX) else {
        return Ok(Vec::new());
    };
    harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_readfs_err)
}

/// Sortiert gefundene cgroup-Kandidaten nach Namen und kürzt sie auf
/// [`MAX_CGROUPS`] Einträge.
///
/// # Description
/// Sortierung vor der Kürzung macht das Ergebnis unabhängig von der
/// dateisystemabhängigen Auflistungsreihenfolge — deterministisch bei
/// gleichem Fixture-Inhalt und vorhersagbar für einen Betreiber, der weiß,
/// welche cgroups bei einer Überschreitung der Grenze erhalten bleiben (die
/// alphabetisch ersten), analog zu `harw-dod-netcounters`s
/// `cap_and_sort_interfaces`.
///
/// # Arguments
/// - `candidates` (`Vec<PathBuf>`): alle in einem Poll gefundenen
///   Kandidatenpfade, in beliebiger Reihenfolge.
///
/// # Returns
/// Höchstens [`MAX_CGROUPS`] Einträge aus `candidates`, aufsteigend nach dem
/// Dateinamen sortiert.
///
/// # Errors
/// Keine — eine totale Funktion.
///
/// # Examples
/// ```rust
/// use harw_dod_cgroup::sensor::cap_and_sort_cgroups;
/// use std::path::PathBuf;
///
/// let kept = cap_and_sort_cgroups(vec![PathBuf::from("/root/b"), PathBuf::from("/root/a")]);
/// assert_eq!(kept, vec![PathBuf::from("/root/a"), PathBuf::from("/root/b")]);
/// ```
#[must_use]
pub fn cap_and_sort_cgroups(mut candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    candidates.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    candidates.truncate(MAX_CGROUPS);
    candidates
}

// Der rohe, unbereinigte cgroup-Name: der Verzeichnisname des Kandidaten,
// oder `FALLBACK_LABEL`, falls er aus irgendeinem Grund nicht als UTF-8
// lesbar ist (praktisch unerreichbar bei gültigen Linux-Verzeichnisnamen,
// aber total statt `unwrap`).
fn cgroup_name(dir: &Path) -> &str {
    dir.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(FALLBACK_LABEL)
}

/// Reduziert einen cgroup-Namen auf sichere Metriknamen-Zeichen.
///
/// # Description
/// Ein cgroup-Name ist vollständig angreiferkontrolliert (ein
/// Containername kommt von dem, der den Container startet) und landet
/// unmittelbar in einem Metriknamen. Alle ASCII-alphanumerischen Zeichen
/// werden kleingeschrieben übernommen, jedes andere Zeichen (Punkte,
/// Semikola, Anführungszeichen, Leerzeichen — alles, was ein
/// Linux-Verzeichnisname außer `/` und `NUL` tragen darf) wird durch `_`
/// ersetzt, das Ergebnis auf [`MAX_LABEL_LEN`] Zeichen gekürzt. Analog zu
/// `harw-dod-thermal`s Zonenlabel- und `harw-dod-netcounters`s
/// Schnittstellenlabel-Sanitisierung.
///
/// # Arguments
/// - `raw` (`&str`): der rohe, aus dem Verzeichnisnamen gelesene
///   cgroup-Name, vor jeder Bereinigung.
///
/// # Returns
/// Ein sanitisiertes Label aus `[a-z0-9_]`, nie leer (siehe
/// [`FALLBACK_LABEL`]).
///
/// # Errors
/// Keine — eine totale Funktion.
///
/// # Examples
/// ```rust
/// use harw_dod_cgroup::sensor::sanitize_label;
///
/// assert_eq!(sanitize_label("my.evil;name"), "my_evil_name");
/// assert_eq!(sanitize_label("system.slice"), "system_slice");
/// ```
#[must_use]
pub fn sanitize_label(raw: &str) -> String {
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
        FALLBACK_LABEL.to_owned()
    } else {
        sanitized
    }
}

// Liest eine Kontrolldatei, die einen einzelnen `u64`-Skalar trägt
// (`memory.current`, `pids.current`). Eine fehlende, nicht als Verzeichnis
// auflösbare oder aus einem anderen Grund nicht zugreifbare Datei liefert
// `Ok(None)` — der zugehörige Controller ist für diese cgroup nicht
// aktiviert, das ist der gesunde Normalfall (siehe `crate`-Moduldoku,
// Entscheidung 3). Eine vorhandene, aber fehlerhaft geformte Datei liefert
// `Err(MalformedSource)` und bricht den gesamten Abruf ab.
fn read_optional_u64(scope: &ReadScope, path: &Path) -> Result<Option<u64>, SensorError> {
    match harw_dod_readfs::parse_u64(scope, path) {
        Ok(value) => Ok(Some(value)),
        Err(ReadFsError::Scope(SensorError::Io(_))) => Ok(None),
        Err(ReadFsError::Scope(inner)) => Err(inner),
        Err(ReadFsError::TooLarge { .. }) => Err(SensorError::MalformedSource),
        Err(ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. }) => {
            // parse_u64 ruft nie glob() auf; unerreichbar, aber erschöpfend
            // abgedeckt, damit eine künftige ReadFsError-Variante hier nicht
            // still verworfen wird.
            Err(SensorError::MalformedSource)
        }
    }
}

// Liest `memory.max`: entweder eine `u64`-Zahl oder das Literal `max`, das
// als `u64::MAX` gemeldet wird (siehe `crate`-Moduldoku, Entscheidung 2).
// Fehlend/nicht zugreifbar → `Ok(None)`, analog zu `read_optional_u64`.
// Vorhanden, aber weder `max` noch eine gültige `u64`-Zahl →
// `Err(MalformedSource)`.
fn read_memory_max(scope: &ReadScope, path: &Path) -> Result<Option<u64>, SensorError> {
    match harw_dod_readfs::read_first_line(scope, path) {
        Ok(line) => {
            let trimmed = line.trim();
            if trimmed == MEMORY_MAX_UNLIMITED_LITERAL {
                Ok(Some(u64::MAX))
            } else {
                trimmed
                    .parse::<u64>()
                    .map(Some)
                    .map_err(|_| SensorError::MalformedSource)
            }
        }
        Err(ReadFsError::Scope(SensorError::Io(_))) => Ok(None),
        Err(ReadFsError::Scope(inner)) => Err(inner),
        Err(ReadFsError::TooLarge { .. }) => Err(SensorError::MalformedSource),
        Err(ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. }) => {
            Err(SensorError::MalformedSource)
        }
    }
}

// Liest `cpu.stat` und sucht darin den Wert des Schlüssels
// `CPU_STAT_USAGE_KEY`. Fehlend/nicht zugreifbar → `Ok(None)`, analog zu
// `read_optional_u64`. Vorhanden, aber ohne diesen Schlüssel oder mit einem
// nicht numerischen Wert → `Err(MalformedSource)`.
fn read_cpu_usage_usec(scope: &ReadScope, path: &Path) -> Result<Option<u64>, SensorError> {
    match harw_dod_readfs::read_key_values(scope, path, CPU_STAT_SEPARATOR) {
        Ok(pairs) => {
            let usage = pairs
                .iter()
                .find(|(key, _)| key == CPU_STAT_USAGE_KEY)
                .map(|(_, value)| value.as_str())
                .ok_or(SensorError::MalformedSource)?;
            usage
                .trim()
                .parse::<u64>()
                .map(Some)
                .map_err(|_| SensorError::MalformedSource)
        }
        Err(ReadFsError::Scope(SensorError::Io(_))) => Ok(None),
        Err(ReadFsError::Scope(inner)) => Err(inner),
        Err(ReadFsError::TooLarge { .. }) => Err(SensorError::MalformedSource),
        Err(ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. }) => {
            Err(SensorError::MalformedSource)
        }
    }
}

// Baut einen `HostSample` aus Metrikname und sanitisiertem Label.
fn build_sample(
    sensor_id: &SensorId,
    now: Timestamp,
    metric_name: &'static str,
    label: &str,
    value: f64,
) -> HostSample {
    HostSample {
        sensor: sensor_id.clone(),
        observed_at: now,
        metric: Cow::Owned(format!("{metric_name}_{label}")),
        value,
    }
}

// Bildet einen `harw_dod_readfs::ReadFsError` auf `SensorError` ab. Analog
// zur privaten `map_readfs_err`-Hilfsfunktion in `harw-dod-thermal`.
fn map_readfs_err(err: ReadFsError) -> SensorError {
    match err {
        ReadFsError::Scope(inner) => inner,
        ReadFsError::TooLarge { .. }
        | ReadFsError::GlobPatternAbsolute { .. }
        | ReadFsError::GlobPatternTraversal { .. } => SensorError::MalformedSource,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    use harw_dod_signals::Sensor;
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::{CgroupSensor, MAX_CARDINALITY, MAX_CGROUPS, cap_and_sort_cgroups, sanitize_label};

    /// Das `fixtures/`-Wurzelverzeichnis dieser Crate.
    fn fixtures_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    /// Baut einen `CgroupSensor`, dessen `ReadScope` genau `root` umfasst.
    fn build_sensor(root: PathBuf) -> CgroupSensor {
        let scope = ReadScope::from_roots([root]);
        let handle = SensorHandle::new(
            SensorId::from_str("cgroup-test"),
            Capability::ReadCgroupV2,
        )
        .bind(scope);
        CgroupSensor::from(handle)
    }

    #[test]
    fn test_max_cardinality_matches_max_cgroups_times_field_count() {
        assert_eq!(MAX_CARDINALITY, MAX_CGROUPS * 4);
        assert_eq!(MAX_CARDINALITY, 64);
    }

    #[test]
    fn test_capability_and_sensor_id_constants() {
        assert_eq!(CgroupSensor::CAPABILITY, Capability::ReadCgroupV2);
        assert_eq!(CgroupSensor::SENSOR_ID, "cgroup");
    }

    #[test]
    fn test_sanitize_label_lowercases_and_keeps_alnum() {
        assert_eq!(sanitize_label("System.Slice"), "system_slice");
    }

    #[test]
    fn test_sanitize_label_never_reproduces_hostile_separators() {
        // Ein absichtlich feindlicher, injection-artiger cgroup-Name: Punkt
        // und Semikolon dürfen im sanitisierten Label nicht mehr auftauchen.
        let label = sanitize_label("my.evil;name");
        assert_eq!(label, "my_evil_name");
        assert!(!label.contains('.'));
        assert!(!label.contains(';'));
    }

    #[test]
    fn test_sanitize_label_truncates_and_never_empty() {
        let long = "a".repeat(200);
        let label = sanitize_label(&long);
        assert_eq!(label.len(), 64);

        assert_eq!(sanitize_label("   "), "cgroup");
        assert_eq!(sanitize_label(""), "cgroup");
    }

    #[test]
    fn test_cap_and_sort_cgroups_truncates_alphabetically() {
        let mut many: Vec<PathBuf> = (0..20)
            .map(|i| PathBuf::from(format!("/root/cg{i:02}")))
            .collect();
        many.reverse(); // absichtlich unsortiert übergeben
        let kept = cap_and_sort_cgroups(many);

        assert_eq!(kept.len(), MAX_CGROUPS);
        assert_eq!(kept[0], PathBuf::from("/root/cg00"));
        assert_eq!(kept[15], PathBuf::from("/root/cg15"));
    }

    #[test]
    fn test_unlimited_memory_max_is_reported_not_malformed() {
        let sensor = build_sensor(fixtures_root().join("unlimited-memory/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("memory.max = \"max\" darf keinen Fehler auslösen");

        let unlimited = reading
            .samples
            .iter()
            .find(|s| s.metric.starts_with("memory_max_bytes_"))
            .expect("memory_max_bytes-Sample muss vorhanden sein");
        assert!(
            (unlimited.value - u64::MAX as f64).abs() < 1.0,
            "memory.max = \"max\" muss als u64::MAX gemeldet werden, war {}",
            unlimited.value
        );
    }

    #[test]
    fn test_result_never_contains_process_information() {
        // Belegt die Nebenläufigkeits-/Redaktions-Auflage: kein
        // Prozessname, keine Kommandozeile, keine PID-Liste im
        // serialisierten Ergebnis — dieser Sensor liest `cgroup.procs`
        // nirgends.
        let sensor = build_sensor(fixtures_root().join("typical/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("typical Fixture muss erfolgreich pollen");
        let json = serde_json::to_string(&reading.samples).expect("Samples serialisieren");

        assert!(!json.contains("cgroup.procs"));
        assert!(!json.contains("\"pid\""));
        assert!(!json.contains("\"cmdline\""));
        assert!(!json.contains("\"comm\""));
    }

    #[test]
    fn test_missing_controller_file_skips_only_that_metric() {
        // Eine cgroup, für die nur der `memory`-Controller aktiviert ist
        // (nur `memory.current` vorhanden), darf trotzdem gemeldet werden —
        // ohne die drei fehlenden Metriken zu erfinden.
        let sensor = build_sensor(fixtures_root().join("many-cgroups/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("many-cgroups Fixture muss erfolgreich pollen");

        assert!(
            reading
                .samples
                .iter()
                .all(|s| s.metric.starts_with("memory_current_bytes_")),
            "many-cgroups-Fixture trägt nur memory.current je cgroup"
        );
        assert_eq!(reading.samples.len(), MAX_CGROUPS);
    }

    harw_dod_fixtures::sensor_suite! {
        sensor: CgroupSensor,
        capability: Capability::ReadCgroupV2,
        max_cardinality: MAX_CARDINALITY,
        fixtures: "fixtures",
    }
}
