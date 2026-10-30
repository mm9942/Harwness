//! `GpuSensor`: der `Sensor`-Trait-Impl dieser Crate (AW2-12-Brief).
//!
//! Siehe die Crate-Moduldoku (`lib.rs`) für Zweck, sysfs-Format, warum ein
//! Host ohne GPU der Normalfall ist, die herstellerabhängige Fläche, die
//! Millidegree-Umrechnung, Kartenlabel/Kardinalität und die Begründung,
//! warum dieser Sensor `Sensor` von Hand statt über
//! `#[derive(harw_macros::SensorSource)]` implementiert. Dieses Modul
//! besitzt ausschließlich [`GpuSensor`] und die private Lese-/
//! Umrechnungslogik dahinter; kein `std::fs`-Aufruf steht in dieser Datei —
//! jeder Zugriff läuft über `harw_dod_readfs` (`parse_i64`, `parse_u64`,
//! `glob::glob`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Suffix-Glob-Muster für die Kartengeräteverzeichnisse, relativ zur ersten
/// Bereichswurzel (siehe [`relative_pattern`] für die Begründung, warum das
/// volle Muster erst zur Laufzeit zusammengesetzt wird, statt fest
/// verdrahtet zu sein).
const CARD_DEVICE_GLOB_SUFFIX: &str = "card*/device";

/// Suffix-Glob-Muster für die `hwmon`-Temperaturdatei einer Karte, relativ
/// zum jeweiligen Kartengeräteverzeichnis. Mehrere `hwmon*`-Verzeichnisse
/// unter einer Karte sind auf realer Hardware unüblich; existiert dennoch
/// mehr als eines, wählt [`read_optional_hwmon_millicelsius`] den ersten,
/// sortierten Treffer (dieselbe Determinismus-Garantie wie
/// `harw_dod_readfs::glob::glob` selbst).
const HWMON_TEMP_GLOB_SUFFIX: &str = "hwmon/hwmon*/temp1_input";

/// Dateiname der Auslastungsmetrik (amdgpu) innerhalb eines
/// Kartengeräteverzeichnisses.
const BUSY_PERCENT_FILE: &str = "gpu_busy_percent";

/// Dateiname des belegten Grafikspeichers in Bytes (amdgpu) innerhalb eines
/// Kartengeräteverzeichnisses.
const VRAM_USED_FILE: &str = "mem_info_vram_used";

/// Dateiname des gesamten Grafikspeichers in Bytes (amdgpu) innerhalb eines
/// Kartengeräteverzeichnisses.
const VRAM_TOTAL_FILE: &str = "mem_info_vram_total";

/// Umrechnungsfaktor sysfs-Millidegree-Celsius → Grad Celsius (siehe
/// `lib.rs`-Moduldoku, Abschnitt „Einheitenumrechnung").
const MILLICELSIUS_PER_CELSIUS: f64 = 1000.0;

/// Feste Metrikpräfixe; das sanitisierte Kartenlabel wird direkt angehängt
/// (siehe `lib.rs`-Moduldoku, Abschnitt „Kartenlabel und Kardinalität").
const BUSY_METRIC_PREFIX: &str = "gpu_busy_percent_";
const VRAM_USED_METRIC_PREFIX: &str = "gpu_vram_used_bytes_";
const VRAM_TOTAL_METRIC_PREFIX: &str = "gpu_vram_total_bytes_";
const TEMPERATURE_METRIC_PREFIX: &str = "gpu_temperature_celsius_";

/// Deklarierte Obergrenze der Kartenzahl je Host — siehe `lib.rs`-Moduldoku,
/// Abschnitt „Kartenlabel und Kardinalität", für die Begründung: klein und
/// über die Laufzeit stabil, analog zu `harw-dod-thermal`s Zonen-Obergrenze.
pub(crate) const MAX_CARDS: usize = 16;

/// Zahl der möglichen Metriken je Karte (Auslastung, zwei Speicherwerte,
/// Temperatur) — unabhängig davon, wie viele davon ein konkreter Treiber
/// tatsächlich exponiert.
const METRICS_PER_CARD: usize = 4;

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll, der
/// Wert, den [`harw_dod_fixtures::sensor_suite!`] als `max_cardinality`
/// erhält.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_gpu::sensor::MAX_CARDINALITY, 64);
/// ```
pub const MAX_CARDINALITY: usize = MAX_CARDS * METRICS_PER_CARD;

/// Obergrenze der Zeichen, die aus dem Kartenverzeichnisnamen in den
/// Metriknamen übernommen werden — Schutz gegen unbegrenztes Wachstum durch
/// einen ungewöhnlichen Verzeichnisnamen.
const MAX_LABEL_LEN: usize = 64;

/// Ersatzlabel, falls selbst der Kartenverzeichnisname nicht als UTF-8
/// lesbar ist (praktisch unerreichbar bei sysfs-Kartennamen, aber total
/// statt `unwrap`).
const FALLBACK_CARD_LABEL: &str = "card";

/// Die vier optionalen Rohmesswerte einer Karte, wie tatsächlich sichtbar.
///
/// `None` bedeutet: die zugehörige Datei existiert auf diesem Host nicht
/// (herstellerabhängig, siehe `lib.rs`-Moduldoku) — kein Fehler.
struct CardMetrics {
    busy_percent: Option<u64>,
    vram_used_bytes: Option<u64>,
    vram_total_bytes: Option<u64>,
    temperature_millicelsius: Option<i64>,
}

/// GPU-Zustandssensor: liest die sichtbaren Metriken jeder gefundenen Karte
/// unterhalb der Bereichswurzel.
///
/// # Description
/// Hält ausschließlich den gebundenen Griff; die gesamte Lese- und
/// Umrechnungslogik steht in [`Sensor::poll`]. Implementiert `Sensor` von
/// Hand statt über `#[derive(harw_macros::SensorSource)]` — siehe
/// `lib.rs`-Moduldoku, Abschnitt „Warum kein `#[derive(SensorSource)]`", für
/// die vollständige Begründung.
#[derive(Debug)]
pub struct GpuSensor {
    handle: SensorHandle<Bound>,
}

impl GpuSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration für das CI-Privilegienbudget-Gate,
    /// nachgebildet aus dem, was `#[derive(harw_macros::SensorSource)]` an
    /// derselben Stelle erzeugen würde (siehe `lib.rs`-Moduldoku).
    ///
    /// # Returns
    /// [`Capability::ReadSysfsDrm`], die einzige Fähigkeit dieses Sensors.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_gpu::GpuSensor;
    ///
    /// assert_eq!(GpuSensor::CAPABILITY, Capability::ReadSysfsDrm);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadSysfsDrm;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Rein informativ, analog zu dem, was
    /// `#[derive(harw_macros::SensorSource)]` als `Self::SENSOR_ID` erzeugen
    /// würde. Nicht zu verwechseln mit `harw_dod_cap::SensorHandle::id()`,
    /// der Kennung der konkreten Sensor-*Instanz*.
    ///
    /// # Returns
    /// `"gpu"`.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_gpu::GpuSensor;
    ///
    /// assert_eq!(GpuSensor::SENSOR_ID, "gpu");
    /// ```
    pub const SENSOR_ID: &'static str = "gpu";
}

impl From<SensorHandle<Bound>> for GpuSensor {
    /// Baut einen `GpuSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige von `harw_dod_fixtures::sensor_suite!` verlangte
    /// Konstruktionskonvention (siehe dessen Moduldoku): ein gebundener
    /// Griff hinein, eine Sensor-Instanz heraus.
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, mit
    ///   [`Capability::ReadSysfsDrm`] und einem `ReadScope` auf die
    ///   DRM-Wurzel (real: `/sys/class/drm`; in Tests: ein Fixture-`tree/`-
    ///   Verzeichnis).
    ///
    /// # Returns
    /// Einen einsatzbereiten `GpuSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::AliasRoot;
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_gpu::GpuSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// // `/sys/class/drm/*`-Einträge sind Symlinks nach `/sys/devices/...`
    /// // (F-005) — `AliasRoot::sysfs_class` baut den Bereich, der das
    /// // zulässt, statt der für sysfs-Klassenwurzeln unsicheren
    /// // `ReadScope::from_roots`.
    /// let alias =
    ///     AliasRoot::sysfs_class(PathBuf::from("/sys/class/drm")).expect("gültige sysfs-Klassenwurzel");
    /// let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
    /// let handle = SensorHandle::new(SensorId::from_str("gpu-0"), Capability::ReadSysfsDrm)
    ///     .bind(scope);
    /// let _sensor = GpuSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for GpuSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Description
    /// Reine Referenz-Rückgabe; siehe `harw_dod_signals::Sensor::handle`.
    ///
    /// # Returns
    /// Referenz auf den bei [`GpuSensor::from`] übergebenen `SensorHandle`.
    ///
    /// # Errors
    /// Keine.
    ///
    /// # Examples
    /// Siehe [`GpuSensor::from`].
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest alle sichtbaren Karten einmal aus.
    ///
    /// # Description
    /// Ermittelt zuerst die Kartengeräteverzeichnisse unterhalb der ersten
    /// Bereichswurzel (`card*/device`, siehe [`relative_pattern`] für die
    /// Begründung, warum das Muster zur Laufzeit gebaut wird), verwirft
    /// dabei DRM-Connector-Verzeichnisse wie `card1-HDMI-A-1`
    /// ([`is_card_root_name`], F-203) und kürzt auf [`MAX_CARDS`] echte
    /// Karten. **Kein** gefundenes Kartenverzeichnis ist der Normalfall
    /// „keine GPU auf diesem Host" (siehe `lib.rs`-Moduldoku) und ergibt ein
    /// leeres, erfolgreiches `SensorReading` — keinen Fehler.
    ///
    /// Für jede gefundene Karte liest diese Funktion die vier optionalen
    /// Rohwerte (siehe [`read_card_metrics`]): eine fehlende Datei wird
    /// stillschweigend ausgelassen (herstellerabhängig, kein Fehler); ein
    /// anderer E/A-Fehler oder ein nicht parsbarer Inhalt bricht den
    /// gesamten Abruf ab (siehe `lib.rs`-Moduldoku, Abschnitt „Ein Host ohne
    /// GPU ist der Normalfall", für die volle Unterscheidung). Die
    /// Temperatur wird von Millidegree- in Grad Celsius umgerechnet (siehe
    /// `lib.rs`-Moduldoku, Abschnitt „Einheitenumrechnung"). Jede vorhandene
    /// Metrik erzeugt genau ein `HostSample`, dessen `metric` das
    /// Kartenlabel trägt.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert in jedes
    ///   `HostSample::observed_at` übernommen. Diese Funktion liest nie die
    ///   Systemuhr.
    ///
    /// # Returns
    /// Ein `SensorReading` mit bis zu vier `HostSample`s je gefundener
    /// Karte und keinen Events. **Leer bei `Ok`, wenn keine Karte gefunden
    /// wurde** — anders als bei den meisten anderen Sensor-Crates dieses
    /// Ausbauprogramms ist das kein Sonderfall, den ein Aufrufer gesondert
    /// behandeln müsste.
    ///
    /// # Errors
    /// - [`SensorError::MalformedSource`]: eine gefundene Kartendatei ist
    ///   leer oder ihr Inhalt ist nicht als Zahl parsbar. Bricht den
    ///   gesamten Abruf ab, statt die defekte Karte stillschweigend
    ///   auszulassen — anders als eine fehlende Datei ist ein unparsbarer
    ///   Inhalt kein herstellerbedingter, sondern ein struktureller Defekt.
    /// - [`SensorError::Io`]: ein Betriebssystemfehler beim Lesen einer
    ///   gefundenen Kartendatei, der kein „Datei fehlt" ist (z. B.
    ///   Berechtigung verweigert) — siehe `lib.rs`-Moduldoku für die
    ///   Unterscheidung „keine GPU" vs. „GPU da, aber unlesbar".
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::scope::AliasRoot;
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_gpu::GpuSensor;
    /// use harw_dod_signals::Sensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let alias =
    ///     AliasRoot::sysfs_class(PathBuf::from("/sys/class/drm")).expect("gültige sysfs-Klassenwurzel");
    /// let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
    /// let handle = SensorHandle::new(SensorId::from_str("gpu-0"), Capability::ReadSysfsDrm)
    ///     .bind(scope);
    /// let sensor = GpuSensor::from(handle);
    /// let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
    /// # Ok::<(), harw_dod_cap::SensorError>(())
    /// ```
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let device_dirs = card_device_dirs(scope)?;

        let mut samples = Vec::with_capacity(device_dirs.len() * METRICS_PER_CARD);
        for device_dir in &device_dirs {
            let label = card_label(device_dir);
            let metrics = read_card_metrics(scope, device_dir)?;

            if let Some(value) = metrics.busy_percent {
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{BUSY_METRIC_PREFIX}{label}")),
                    value: value as f64,
                });
            }
            if let Some(value) = metrics.vram_used_bytes {
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{VRAM_USED_METRIC_PREFIX}{label}")),
                    value: value as f64,
                });
            }
            if let Some(value) = metrics.vram_total_bytes {
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{VRAM_TOTAL_METRIC_PREFIX}{label}")),
                    value: value as f64,
                });
            }
            if let Some(millicelsius) = metrics.temperature_millicelsius {
                let celsius = millicelsius as f64 / MILLICELSIUS_PER_CELSIUS;
                samples.push(HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("{TEMPERATURE_METRIC_PREFIX}{label}")),
                    value: celsius,
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

// Baut ein bereichsrelatives Glob-Muster aus `base` und `suffix`, statt den
// vollen Pfad fest zu verdrahten. `harw_dod_readfs::glob::glob` beginnt
// seine Suche laut eigener Moduldoku immer bei `/` und nutzt den `ReadScope`
// nur als nachträglichen Filter — ein zur Kompilierzeit fester Pfad wie
// `"sys/class/drm/card*/device"` fände deshalb in einer Fixture-Prüfung
// (Bereichswurzel = ein `fixtures/<fall>/tree`-Verzeichnis) nie etwas, weil
// dort nie gesucht würde. Generisch über `base`, weil sowohl die eigentliche
// Bereichswurzel (`card_device_dirs`) als auch ein bereits gefundenes
// Kartengeräteverzeichnis (`read_optional_hwmon_millicelsius`) als
// Ausgangspunkt für ein weiteres, tieferes Glob-Muster dienen. `None`, wenn
// `base` nicht absolut ist — ein entartetes, in der Praxis unerreichbares
// Konfigurationsproblem.
fn relative_pattern(base: &Path, suffix: &str) -> Option<String> {
    let relative = base.strip_prefix("/").ok()?;
    let relative = relative.to_string_lossy();
    if relative.is_empty() {
        Some(suffix.to_owned())
    } else {
        Some(format!("{relative}/{suffix}"))
    }
}

// Findet alle Kartengeräteverzeichnisse innerhalb der ersten Bereichswurzel
// von `scope`, sortiert (siehe `glob::glob`s eigene Garantie). Ein `scope`
// ohne Wurzeln, mit nicht-absoluter Wurzel, oder ohne sichtbare Karten
// liefert eine leere Liste — **kein** Fehler, siehe `lib.rs`-Moduldoku,
// Abschnitt „Ein Host ohne GPU ist der Normalfall".
//
// F-203: `card*` matcht auch Connector-Verzeichnisse (`card1-HDMI-A-1`, die
// DRM für jeden Anschluss einer Karte zusätzlich unter `/sys/class/drm`
// anlegt, ebenfalls mit einem eigenen `device`-Symlink zurück zur Karte).
// Ohne Filterung würde ein Multi-Monitor-Host dieselbe physische Karte
// mehrfach unter verschiedenen Labels melden. Echte Kartenverzeichnisse
// heißen ausschließlich `card` gefolgt von Ziffern (`card0`, `card1`, …) —
// [`is_card_root_name`] verwirft alles mit einem weiteren Suffix. Zusätzlich
// wird hier — anders als zuvor, wo die Konstante nur deklariert, aber nie
// durchgesetzt wurde — auf [`MAX_CARDS`] gekürzt (Ergebnis bleibt
// alphabetisch sortiert, siehe `glob::glob`s eigene Garantie).
fn card_device_dirs(scope: &ReadScope) -> Result<Vec<PathBuf>, SensorError> {
    let Some(root) = scope.roots().next() else {
        return Ok(Vec::new());
    };
    let Some(pattern) = relative_pattern(root, CARD_DEVICE_GLOB_SUFFIX) else {
        return Ok(Vec::new());
    };
    let mut matches = harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_readfs_err)?;
    matches.retain(|device_dir| {
        device_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(is_card_root_name)
    });
    matches.truncate(MAX_CARDS);
    Ok(matches)
}

/// Ist `name` der Name eines echten Kartenverzeichnisses (`card0`, `card12`,
/// …) und nicht eines DRM-Connector-Verzeichnisses (`card1-HDMI-A-1`)?
///
/// # Description
/// Ein echtes Kartenverzeichnis heißt exakt `card` gefolgt von mindestens
/// einer ASCII-Ziffer, ohne weiteres Suffix (F-203). Ein
/// Connector-Verzeichnis trägt nach der Kartennummer stets einen
/// Bindestrich, gefolgt vom Anschlussnamen (`-HDMI-A-1`, `-DP-1`, …) — genau
/// dieses zusätzliche, nicht rein numerische Suffix verwirft diese Funktion.
///
/// # Arguments
/// - `name` (`&str`): der zu prüfende Verzeichnisname (der Elternteil eines
///   `card*/device`-Treffers).
///
/// # Returns
/// `true`, wenn `name` ausschließlich aus `"card"` plus Ziffern besteht.
///
/// # Examples
/// ```rust
/// use harw_dod_gpu::sensor::is_card_root_name;
///
/// assert!(is_card_root_name("card0"));
/// assert!(is_card_root_name("card12"));
/// assert!(!is_card_root_name("card1-HDMI-A-1"));
/// assert!(!is_card_root_name("card"));
/// assert!(!is_card_root_name("cardX"));
/// ```
#[must_use]
pub fn is_card_root_name(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
}

// Liest die vier optionalen Rohwerte einer Karte. Jede einzelne Datei ist
// unabhängig optional (herstellerabhängig, siehe `lib.rs`-Moduldoku); nur
// ein tatsächlicher Lese- oder Formatfehler auf einer *gefundenen* Datei
// bricht die gesamte Funktion ab (siehe [`classify_read_error`]).
fn read_card_metrics(scope: &ReadScope, device_dir: &Path) -> Result<CardMetrics, SensorError> {
    Ok(CardMetrics {
        busy_percent: read_optional_u64(scope, &device_dir.join(BUSY_PERCENT_FILE))?,
        vram_used_bytes: read_optional_u64(scope, &device_dir.join(VRAM_USED_FILE))?,
        vram_total_bytes: read_optional_u64(scope, &device_dir.join(VRAM_TOTAL_FILE))?,
        temperature_millicelsius: read_optional_hwmon_millicelsius(scope, device_dir)?,
    })
}

// Liest einen optionalen `u64`-Rohwert. `Ok(None)` bedeutet: die Datei fehlt
// auf diesem Host (herstellerabhängig) — kein Fehler. Jeder andere Fehler
// (E/A jenseits „nicht gefunden", nicht parsbarer Inhalt) propagiert.
fn read_optional_u64(scope: &ReadScope, path: &Path) -> Result<Option<u64>, SensorError> {
    match harw_dod_readfs::parse_u64(scope, path) {
        Ok(value) => Ok(Some(value)),
        Err(err) => match classify_read_error(err) {
            None => Ok(None),
            Some(mapped) => Err(mapped),
        },
    }
}

// Wie [`read_optional_u64`], aber für `i64` — für die Millidegree-Rohwerte
// aus `hwmon`.
fn read_optional_i64(scope: &ReadScope, path: &Path) -> Result<Option<i64>, SensorError> {
    match harw_dod_readfs::parse_i64(scope, path) {
        Ok(value) => Ok(Some(value)),
        Err(err) => match classify_read_error(err) {
            None => Ok(None),
            Some(mapped) => Err(mapped),
        },
    }
}

// Findet die `hwmon`-Temperaturdatei einer Karte per Glob (der Dateiname
// liegt hinter einem numerierten `hwmon*`-Verzeichnis, das je Host anders
// heißen kann) und liest sie als optionalen Millidegree-Rohwert. Kein
// Treffer bedeutet: diese Karte hat kein `hwmon`-Verzeichnis — kein Fehler.
//
// Bekannte Grenze: `harw_dod_readfs::glob::glob` behandelt laut eigener
// Moduldoku ein während der Suche unlesbares Verzeichnis (fehlt, keine
// Berechtigung) einheitlich als „kein Treffer", nie als Fehler. Ein
// Berechtigungsproblem beim Auflisten des `hwmon`-Verzeichnisses selbst ist
// über diesen Weg deshalb nicht von einem tatsächlich fehlenden
// `hwmon`-Verzeichnis unterscheidbar — anders als bei den drei direkt über
// `device_dir` gelesenen Dateien (siehe `lib.rs`-Moduldoku, Abschnitt „Ein
// Host ohne GPU ist der Normalfall"). Eine Einschränkung von
// `harw_dod_readfs::glob`, nicht etwas, das diese Crate umgehen könnte.
fn read_optional_hwmon_millicelsius(
    scope: &ReadScope,
    device_dir: &Path,
) -> Result<Option<i64>, SensorError> {
    let Some(pattern) = relative_pattern(device_dir, HWMON_TEMP_GLOB_SUFFIX) else {
        return Ok(None);
    };
    let matches = harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_readfs_err)?;
    let Some(temp_path) = matches.into_iter().next() else {
        return Ok(None);
    };
    read_optional_i64(scope, &temp_path)
}

// Ordnet einen `harw_dod_readfs::ReadFsError` aus einem direkten Lesezugriff
// (nicht aus `glob::glob`) ein: `None` heißt „die Datei fehlt schlicht,
// kein Fehler"; `Some(err)` heißt „propagieren". Nur
// `std::io::ErrorKind::NotFound` gilt als „fehlt" — jeder andere E/A-Fehler
// (z. B. Berechtigung verweigert) propagiert als [`SensorError::Io`] statt
// wie eine fehlende Datei behandelt zu werden (siehe `lib.rs`-Moduldoku,
// Abschnitt „Ein Host ohne GPU ist der Normalfall", für die Begründung
// dieser Unterscheidung).
fn classify_read_error(err: ReadFsError) -> Option<SensorError> {
    match err {
        ReadFsError::Scope(SensorError::Io(io_err))
            if io_err.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        ReadFsError::Scope(inner) => Some(inner),
        ReadFsError::TooLarge { .. } => Some(SensorError::MalformedSource),
        ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. } => {
            Some(SensorError::MalformedSource)
        }
        // Wie `TooLarge`: eine überschrittene Glob-Grenze beschreibt eine
        // Quelle mit unerwarteter Form, nicht einen Fehler dieses Werkzeugs
        // (C-SCOPE-Nachfolge, F-005-Register).
        ReadFsError::GlobLimitExceeded { .. } => Some(SensorError::MalformedSource),
    }
}

// Bildet einen `harw_dod_readfs::ReadFsError` aus einem `glob::glob`-Aufruf
// auf `SensorError` ab. `glob::glob` erzeugt laut eigener Moduldoku nie
// `Scope`- oder `TooLarge`-Varianten — nur die beiden Musterfehler, die hier
// nur durch einen Fehler in der internen Mustererzeugung dieser Crate
// erreichbar wären, nicht durch Hostzustand.
fn map_readfs_err(err: ReadFsError) -> SensorError {
    match err {
        ReadFsError::Scope(inner) => inner,
        ReadFsError::TooLarge { .. }
        | ReadFsError::GlobPatternAbsolute { .. }
        | ReadFsError::GlobPatternTraversal { .. }
        | ReadFsError::GlobLimitExceeded { .. } => SensorError::MalformedSource,
    }
}

// Bestimmt das Label einer Karte: der sanitisierte Name ihres
// `card*`-Verzeichnisses (der Elternteil von `device_dir`), z. B. `card0`.
fn card_label(device_dir: &Path) -> String {
    let raw = device_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(FALLBACK_CARD_LABEL);
    sanitize_label(raw)
}

// Reduziert ein aus einem Verzeichnisnamen stammendes Label auf sichere
// Metriknamen-Zeichen (`[a-z0-9_]`), gekürzt auf `MAX_LABEL_LEN` Zeichen.
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
        FALLBACK_CARD_LABEL.to_owned()
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

    use super::{GpuSensor, MAX_CARDS, card_device_dirs, is_card_root_name};
    use crate::test_support::{TestError, TestResult, ctx};

    /// Das `fixtures/`-Wurzelverzeichnis dieser Crate.
    fn fixtures_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    /// Baut einen `GpuSensor`, dessen `ReadScope` genau `root` umfasst.
    fn build_sensor(root: PathBuf) -> GpuSensor {
        let scope = ReadScope::from_roots([root]);
        let handle =
            SensorHandle::new(SensorId::from_str("gpu-test"), Capability::ReadSysfsDrm).bind(scope);
        GpuSensor::from(handle)
    }

    #[test]
    fn test_host_without_gpu_yields_empty_reading_not_an_error() -> TestResult {
        // Der wichtigste Test dieser Crate (siehe `lib.rs`-Moduldoku): ein
        // leerer Baum ohne jede Karte darf den Abruf nicht scheitern lassen.
        let sensor = build_sensor(fixtures_root().join("empty-host/tree"));
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "ein Host ohne GPU muss erfolgreich pollen, nicht scheitern",
        ))?;

        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_partial_card_reports_present_files_without_failing() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("partial-card/tree"));
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "eine Karte mit nur einem Teil der Dateien darf den Abruf nicht scheitern lassen",
        ))?;

        // Nur `gpu_busy_percent` und die `hwmon`-Temperatur sind in diesem
        // Fixture vorhanden — kein `mem_info_vram_used`/`_total`.
        assert_eq!(reading.samples.len(), 2);
        assert!(
            reading
                .samples
                .iter()
                .any(|s| s.metric.starts_with("gpu_busy_percent_")),
            "die vorhandene Auslastungsmetrik muss gemeldet werden: {:?}",
            reading.samples
        );
        assert!(
            reading
                .samples
                .iter()
                .any(|s| s.metric.starts_with("gpu_temperature_celsius_")),
            "die vorhandene Temperaturmetrik muss gemeldet werden: {:?}",
            reading.samples
        );
        assert!(
            reading
                .samples
                .iter()
                .all(|s| !s.metric.starts_with("gpu_vram_")),
            "eine fehlende VRAM-Datei darf keine Metrik erzeugen: {:?}",
            reading.samples
        );
        Ok(())
    }

    #[test]
    fn test_millicelsius_is_converted_to_celsius() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("one-card-full/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("one-card-full Fixture muss erfolgreich pollen"))?;

        let temp_sample = reading
            .samples
            .iter()
            .find(|s| s.metric.starts_with("gpu_temperature_celsius_"))
            .ok_or(TestError::Missing("Temperatursample"))?;

        // Epsilon-Vergleich statt `assert_eq!` auf `f64`: 52000/1000 = 52.0
        // ist hier zwar exakt darstellbar, ein direkter `==`-Vergleich auf
        // Gleitkommazahlen ist aber unabhängig davon ein Muster, das diese
        // Crate nicht etablieren will.
        assert!(
            (temp_sample.value - 52.0).abs() < f64::EPSILON,
            "52000 Millidegree müssen 52.0 °C ergeben, war {}",
            temp_sample.value
        );
        Ok(())
    }

    #[test]
    fn test_two_cards_yield_distinct_metric_labels() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("two-cards/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("two-cards Fixture muss erfolgreich pollen"))?;

        assert_eq!(reading.samples.len(), 8);
        let card0_busy = reading
            .samples
            .iter()
            .find(|s| s.metric.starts_with("gpu_busy_percent_"))
            .ok_or(TestError::Missing("Auslastungsmetrik"))?;
        assert!(
            reading
                .samples
                .iter()
                .any(|s| s.metric.starts_with("gpu_busy_percent_")
                    && s.metric != card0_busy.metric),
            "zwei Karten müssen unterschiedliche Auslastungs-Metriknamen tragen: {:?}",
            reading.samples
        );
        Ok(())
    }

    #[test]
    fn test_non_numeric_content_is_malformed_source_without_leaking_content() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("malformed/non-numeric"));
        let result = sensor.poll(Timestamp::UNIX_EPOCH);
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "nicht-numerischer Inhalt muss scheitern".to_owned(),
            ));
        };

        assert!(matches!(err, SensorError::MalformedSource));
        let rendered = format!("{err}{err:?}");
        assert!(
            !rendered.contains("not-a-number"),
            "Fehlermeldung darf den gelesenen Inhalt nicht enthalten: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn test_empty_file_content_is_malformed_source() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("malformed/empty-file"));
        let result = sensor.poll(Timestamp::UNIX_EPOCH);
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "eine leere Datei muss scheitern".to_owned(),
            ));
        };

        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    #[test]
    fn test_determinism_with_same_now() -> TestResult {
        let sensor = build_sensor(fixtures_root().join("one-card-full/tree"));
        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("erster Poll muss gelingen"))?;
        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("zweiter Poll muss gelingen"))?;

        assert_eq!(
            first, second,
            "zwei Polls mit demselben injizierten now müssen dasselbe Ergebnis liefern"
        );
        Ok(())
    }

    #[test]
    fn test_is_card_root_name_accepts_only_card_plus_digits() {
        assert!(is_card_root_name("card0"));
        assert!(is_card_root_name("card12"));
        assert!(!is_card_root_name("card1-HDMI-A-1"));
        assert!(!is_card_root_name("card"));
        assert!(!is_card_root_name("cardX"));
    }

    #[test]
    fn test_card_device_dirs_excludes_connector_directories_regression_f203() -> TestResult {
        // Ein Multi-Monitor-Host: `card0` ist die echte Karte, `card0-HDMI-A-1`
        // ist DRMs zusätzliches Connector-Verzeichnis für denselben Anschluss
        // — beide tragen ein `device`-Symlink und würden ohne Filterung als
        // zwei Karten gezählt.
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir_all(dir.path().join("card0/device")).map_err(ctx("card0/device"))?;
        std::fs::write(dir.path().join("card0/device/gpu_busy_percent"), "5\n")
            .map_err(ctx("gpu_busy_percent schreiben"))?;
        std::fs::create_dir_all(dir.path().join("card0-HDMI-A-1/device"))
            .map_err(ctx("card0-HDMI-A-1/device"))?;
        std::fs::write(
            dir.path().join("card0-HDMI-A-1/device/gpu_busy_percent"),
            "5\n",
        )
        .map_err(ctx("gpu_busy_percent im Connector-Verzeichnis schreiben"))?;

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let device_dirs =
            card_device_dirs(&scope).map_err(ctx("card_device_dirs darf nicht scheitern"))?;

        assert_eq!(
            device_dirs.len(),
            1,
            "das Connector-Verzeichnis darf nicht als eigene Karte gezählt werden: {device_dirs:?}"
        );
        Ok(())
    }

    #[test]
    fn test_card_device_dirs_truncates_to_max_cards_regression_f203() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        for i in 0..(MAX_CARDS + 4) {
            let card_dir = dir.path().join(format!("card{i}/device"));
            std::fs::create_dir_all(&card_dir).map_err(ctx("card device dir"))?;
            std::fs::write(card_dir.join("gpu_busy_percent"), "1\n")
                .map_err(ctx("gpu_busy_percent schreiben"))?;
        }

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let device_dirs =
            card_device_dirs(&scope).map_err(ctx("card_device_dirs darf nicht scheitern"))?;

        assert_eq!(
            device_dirs.len(),
            MAX_CARDS,
            "MAX_CARDS muss durchgesetzt werden, nicht nur deklariert sein"
        );
        Ok(())
    }

    /// Der Ordner der echten Pi-Captures (`C-FIXT`, `harw-dod-fixtures`),
    /// über den Workspace-Geschwisterpfad erreicht.
    fn rpi5_captures_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../harw-dod-fixtures/captures/rpi5-6.18"
        ))
    }

    /// Regressionstest für F-005/F-202 mit einer echten, auf diesem
    /// Raspberry Pi 5 erhobenen Capture (`gpu.json`): `card0` erscheint dort
    /// als echter Symlink nach `/sys/devices/platform/axi/1002000000.v3d/drm/card0`.
    /// Die Capture trägt nur `device/uevent` (keinen der vier von diesem
    /// Sensor gelesenen Rohwerte) — der Test belegt deshalb, dass die
    /// Alias-Auflösung die echte Karte überhaupt erst findet (vorher:
    /// `SensorError::SourceUnavailable`-artiges Verhalten durch F-005, hier
    /// stattdessen ein leeres, aber erfolgreiches Ergebnis für genau eine
    /// gefundene Karte), nicht die konkreten Metrikwerte.
    #[test]
    fn test_poll_reads_real_pi_capture_through_alias_scope_regression_f005() -> TestResult {
        let manifest_path = rpi5_captures_dir().join("gpu.json");
        let manifest = harw_dod_fixtures::capture_manifest::load(&manifest_path)
            .map_err(ctx("captures/rpi5-6.18/gpu.json muss ladbar sein"))?;

        let tmp = tempfile::tempdir().map_err(ctx("tempdir für die Materialisierung"))?;
        harw_dod_fixtures::capture_manifest::materialize(&manifest, tmp.path())
            .map_err(ctx("materialize muss die echte Symlink-Struktur anlegen"))?;

        let declared = tmp.path().join("sys/class/drm");
        let resolved_prefix = tmp.path().join("sys/devices");
        let alias = harw_dod_cap::scope::AliasRoot::new(declared, resolved_prefix)
            .map_err(ctx("AliasRoot::new mit Tempdir-Wurzeln"))?;
        let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);

        let device_dirs = card_device_dirs(&scope).map_err(ctx(
            "Alias-Scope muss die reale Karte über den Symlink finden",
        ))?;
        assert_eq!(
            device_dirs.len(),
            1,
            "genau eine echte Karte muss über den Symlink gefunden werden: {device_dirs:?}"
        );

        let handle = SensorHandle::new(
            SensorId::from_str("gpu-alias-capture-test"),
            Capability::ReadSysfsDrm,
        )
        .bind(scope);
        let sensor = GpuSensor::from(handle);
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "ein Host mit einer Karte ohne bekannte Metrikdateien darf nicht scheitern",
        ))?;
        assert!(reading.samples.is_empty());
        Ok(())
    }

    harw_dod_fixtures::sensor_suite! {
        sensor: GpuSensor,
        capability: Capability::ReadSysfsDrm,
        max_cardinality: crate::sensor::MAX_CARDINALITY,
        fixtures: "fixtures",
        // Ein Host ohne GPU ist kein Fehler, sondern der Normalfall. Ohne diese
        // Angabe fordert der Harness `Err(SourceUnavailable)` auf einem leeren
        // Bereich — eine Eingabe, die von "keine GPU vorhanden" nicht zu
        // unterscheiden ist. Siehe K48 im Ausbauplan.
        empty_scope: harw_dod_fixtures::harness::EmptyScopeExpectation::EmptySourceIsNormal,
    }
}
